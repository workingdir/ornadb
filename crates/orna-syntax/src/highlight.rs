//! Context-aware token classification for editor highlighting.
//!
//! This module exposes a stable public classification API for Orna source
//! units. Editor integrations use it for semantic highlighting without
//! depending on the private CST implementation.
//!
//! The classifier walks the lossless CST, so declaration names are
//! recognised even in partially edited source. The returned tokens cover
//! every non-whitespace byte of the source in source order.

use std::ops::Range;

use rowan::{NodeOrToken, SyntaxNode, SyntaxToken};

use crate::{
    SyntaxTree,
    parser::{OrnaLanguage, SyntaxKind},
};

/// One classified token in an Orna source unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightToken {
    /// The byte range of this token in the source unit.
    pub range: Range<usize>,
    /// The semantic classification of this token.
    pub kind: HighlightKind,
}

impl HighlightToken {
    fn new(start: usize, end: usize, kind: HighlightKind) -> Self {
        Self {
            range: start..end,
            kind,
        }
    }
}

/// The semantic classification of one source token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightKind {
    /// A language keyword, such as `CREATE` or `RETURNS`.
    Keyword,
    /// A declared type name or scalar type name.
    TypeName,
    /// A declared function name.
    FunctionName,
    /// A parameter, local variable, or other plain identifier.
    VariableName,
    /// A schema or other namespace component of a qualified name.
    NamespaceName,
    /// A field, column, or member name.
    PropertyName,
    /// A single-quoted string literal.
    StringLiteral,
    /// A numeric literal.
    NumberLiteral,
    /// A line or block comment.
    Comment,
    /// An operator such as `:=`, `=`, or `+`.
    Operator,
    /// Punctuation such as `(`, `)`, `,`, or `;`.
    Punctuation,
    /// A double-quoted identifier.
    QuotedIdentifier,
}

/// Classifies every token in one Orna source unit.
///
/// The source is parsed once for classification. Callers that already hold a
/// [`Parse`](crate::Parse) value should use
/// [`Parse::highlight`](crate::Parse::highlight) instead.
pub fn highlight(source: &str) -> Vec<HighlightToken> {
    crate::parse(source).highlight()
}

/// Classifies every token of an already parsed source tree.
pub(crate) fn highlight_tree(tree: &SyntaxTree) -> Vec<HighlightToken> {
    let mut tokens = Vec::new();
    walk_node(tree.root(), &mut None, &mut tokens);
    merge_adjacent(tokens)
}

/// Merges adjacent tokens that share one classification and touch ranges.
///
/// The lexer fragments multi-character operators and numeric literals into
/// single-character tokens. Consumers such as semantic-token providers want
/// one token per literal or operator.
fn merge_adjacent(tokens: Vec<HighlightToken>) -> Vec<HighlightToken> {
    let mut merged: Vec<HighlightToken> = Vec::with_capacity(tokens.len());
    for token in tokens {
        if let Some(last) = merged.last_mut()
            && last.kind == token.kind
            && last.range.end == token.range.start
        {
            last.range.end = token.range.end;
            continue;
        }
        merged.push(token);
    }
    merged
}

/// The expected role of the next identifier inside one CST container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NameRole {
    Type,
    Function,
    Schema,
    Property,
    Variable,
}

impl NameRole {
    const fn kind(self) -> HighlightKind {
        match self {
            Self::Type => HighlightKind::TypeName,
            Self::Function => HighlightKind::FunctionName,
            Self::Schema => HighlightKind::NamespaceName,
            Self::Property => HighlightKind::PropertyName,
            Self::Variable => HighlightKind::VariableName,
        }
    }
}

fn walk_node(
    node: &SyntaxNode<OrnaLanguage>,
    role: &mut Option<NameRole>,
    tokens: &mut Vec<HighlightToken>,
) {
    match node.kind() {
        SyntaxKind::QualifiedName => classify_qualified_name(node, role.take(), tokens),
        SyntaxKind::CreateSchemaStatement => {
            if schema_statement_has_name_role(node) {
                walk_children_with_role(node, Some(NameRole::Schema), tokens)
            } else {
                // The parser routes other `CREATE` forms through the schema
                // statement node, for example `CREATE ROLE` and
                // `CREATE USER`. These forms have no schema name role.
                walk_children_with_role(node, None, tokens)
            }
        }
        SyntaxKind::CreateTypeStatement => {
            walk_children_with_role(node, Some(NameRole::Type), tokens)
        }
        SyntaxKind::CreateServerFunctionStatement | SyntaxKind::CreateClientFunctionStatement => {
            walk_children_with_role(node, Some(NameRole::Function), tokens)
        }
        SyntaxKind::ObjectField | SyntaxKind::ValueField | SyntaxKind::RowsColumn => {
            walk_children_with_role(node, Some(NameRole::Property), tokens)
        }
        SyntaxKind::AlterTypeRenameFieldStatement => walk_alter_statement(node, tokens),
        SyntaxKind::FunctionParameter | SyntaxKind::ClientFunctionParameter => {
            walk_children_with_role(node, Some(NameRole::Variable), tokens)
        }
        SyntaxKind::NamedTypeSpecification
        | SyntaxKind::ReferenceTypeSpecification
        | SyntaxKind::ListTypeSpecification
        | SyntaxKind::SetTypeSpecification
        | SyntaxKind::MapTypeSpecification
        | SyntaxKind::OptionTypeSpecification
        | SyntaxKind::StreamTypeSpecification
        | SyntaxKind::StreamReturnType => {
            walk_children_with_role(node, Some(NameRole::Type), tokens)
        }
        SyntaxKind::StandardLargeObjectTypeSpecification => {
            walk_standard_large_object(node, tokens)
        }
        SyntaxKind::CapabilitySpecification => {
            walk_children_with_role(node, Some(NameRole::Function), tokens)
        }
        SyntaxKind::ClientCallExpression => {
            walk_children_with_role(node, Some(NameRole::Function), tokens)
        }
        SyntaxKind::SqlQueryBody
        | SyntaxKind::SqlInsertBody
        | SyntaxKind::SqlUpdateBody
        | SyntaxKind::SqlDeleteBody => walk_sql_body(node, tokens),
        _ => walk_children_with_role(node, role.take(), tokens),
    }
}

fn walk_children_with_role(
    node: &SyntaxNode<OrnaLanguage>,
    mut role: Option<NameRole>,
    tokens: &mut Vec<HighlightToken>,
) {
    let children: Vec<NodeOrToken<SyntaxNode<OrnaLanguage>, SyntaxToken<OrnaLanguage>>> =
        node.children_with_tokens().collect();
    for index in 0..children.len() {
        classify_child(&children, index, &mut role, tokens);
    }
}

/// Classifies every word in a canonical multi-word scalar type as a type name.
fn walk_standard_large_object(node: &SyntaxNode<OrnaLanguage>, tokens: &mut Vec<HighlightToken>) {
    let children: Vec<NodeOrToken<SyntaxNode<OrnaLanguage>, SyntaxToken<OrnaLanguage>>> =
        node.children_with_tokens().collect();
    let mut role = None;
    for (index, child) in children.iter().enumerate() {
        match child {
            NodeOrToken::Token(token) if token.kind() == SyntaxKind::Word => {
                let (start, end) = token_range(token);
                tokens.push(HighlightToken::new(start, end, HighlightKind::TypeName));
            }
            NodeOrToken::Token(token) => classify_token(&children, index, token, &mut role, tokens),
            NodeOrToken::Node(child) => walk_node(child, &mut role, tokens),
        }
    }
}

fn classify_child(
    children: &[NodeOrToken<SyntaxNode<OrnaLanguage>, SyntaxToken<OrnaLanguage>>],
    index: usize,
    role: &mut Option<NameRole>,
    tokens: &mut Vec<HighlightToken>,
) {
    match &children[index] {
        NodeOrToken::Token(token) => classify_token(children, index, token, role, tokens),
        NodeOrToken::Node(node) => walk_node(node, role, tokens),
    }
}

fn classify_token(
    children: &[NodeOrToken<SyntaxNode<OrnaLanguage>, SyntaxToken<OrnaLanguage>>],
    index: usize,
    token: &SyntaxToken<OrnaLanguage>,
    role: &mut Option<NameRole>,
    tokens: &mut Vec<HighlightToken>,
) {
    let (start, end) = token_range(token);
    match token.kind() {
        SyntaxKind::Whitespace => {}
        SyntaxKind::LineComment | SyntaxKind::BlockComment => {
            tokens.push(HighlightToken::new(start, end, HighlightKind::Comment));
        }
        SyntaxKind::StringLiteral => {
            tokens.push(HighlightToken::new(
                start,
                end,
                HighlightKind::StringLiteral,
            ));
        }
        SyntaxKind::NumberLiteral => {
            tokens.push(HighlightToken::new(
                start,
                end,
                HighlightKind::NumberLiteral,
            ));
        }
        SyntaxKind::QuotedIdentifier => {
            let kind = role
                .take()
                .map_or(HighlightKind::QuotedIdentifier, NameRole::kind);
            tokens.push(HighlightToken::new(start, end, kind));
        }
        SyntaxKind::Dot
        | SyntaxKind::Semicolon
        | SyntaxKind::LeftParenthesis
        | SyntaxKind::RightParenthesis
        | SyntaxKind::Comma => {
            tokens.push(HighlightToken::new(start, end, HighlightKind::Punctuation));
        }
        SyntaxKind::Word => {
            let text = token.text();
            if is_keyword(text) {
                tokens.push(HighlightToken::new(start, end, HighlightKind::Keyword));
                if role.is_none() && keyword_sets_role(text) {
                    *role = keyword_role(text);
                }
            } else if is_scalar_type(text) {
                tokens.push(HighlightToken::new(start, end, HighlightKind::TypeName));
            } else if role.is_some() {
                // A pending role applies to the final component of a dotted
                // name; earlier components stay namespaces.
                if next_significant_kind(children, index + 1) == Some(SyntaxKind::Dot) {
                    tokens.push(HighlightToken::new(
                        start,
                        end,
                        HighlightKind::NamespaceName,
                    ));
                } else {
                    let kind = role.take().expect("pending role").kind();
                    tokens.push(HighlightToken::new(start, end, kind));
                }
            } else {
                let kind = match (
                    previous_significant_kind(children, index),
                    next_significant_kind(children, index + 1),
                ) {
                    (Some(SyntaxKind::Dot), Some(SyntaxKind::LeftParenthesis)) => {
                        HighlightKind::FunctionName
                    }
                    (Some(SyntaxKind::Dot), _) => HighlightKind::PropertyName,
                    (_, Some(SyntaxKind::Dot)) => HighlightKind::NamespaceName,
                    _ => HighlightKind::VariableName,
                };
                tokens.push(HighlightToken::new(start, end, kind));
            }
        }
        SyntaxKind::Other => tokens.push(HighlightToken::new(
            start,
            end,
            classify_other(token.text()),
        )),
        // The lexer never emits node kinds as tokens.
        _ => {}
    }
}

/// Returns true when this keyword introduces a declaration-name role.
fn keyword_sets_role(word: &str) -> bool {
    keyword_role(word).is_some()
}

/// Returns the declaration-name role introduced by one keyword.
fn keyword_role(word: &str) -> Option<NameRole> {
    if word.eq_ignore_ascii_case("FUNCTION") {
        Some(NameRole::Function)
    } else if word.eq_ignore_ascii_case("TYPE") {
        Some(NameRole::Type)
    } else if word.eq_ignore_ascii_case("SCHEMA") {
        Some(NameRole::Schema)
    } else if word.eq_ignore_ascii_case("REF") {
        Some(NameRole::Type)
    } else {
        None
    }
}

/// Classifies one `ALTER TYPE` statement.
///
/// The type name after `ALTER TYPE` is a type name, the old field after
/// `RENAME FIELD` is a property, and the new name after `TO` is a property
/// in a field rename but a type name in a type rename.
fn walk_alter_statement(node: &SyntaxNode<OrnaLanguage>, tokens: &mut Vec<HighlightToken>) {
    let children: Vec<NodeOrToken<SyntaxNode<OrnaLanguage>, SyntaxToken<OrnaLanguage>>> =
        node.children_with_tokens().collect();
    let mut role = None;
    let mut rename_field = false;
    for index in 0..children.len() {
        if let NodeOrToken::Token(token) = &children[index]
            && token.kind() == SyntaxKind::Word
            && is_keyword(token.text())
        {
            if token.text().eq_ignore_ascii_case("TYPE") {
                role = Some(NameRole::Type);
            } else if token.text().eq_ignore_ascii_case("FIELD") {
                rename_field = true;
                role = Some(NameRole::Property);
            } else if token.text().eq_ignore_ascii_case("TO") {
                role = Some(if rename_field {
                    NameRole::Property
                } else {
                    NameRole::Type
                });
            }
        }
        classify_child(&children, index, &mut role, tokens);
    }
}

/// Returns true when this parsed schema statement declares a schema name.
fn schema_statement_has_name_role(node: &SyntaxNode<OrnaLanguage>) -> bool {
    node.children_with_tokens().any(|child| {
        matches!(
            &child,
            NodeOrToken::Token(token) if token.kind() == SyntaxKind::Word
                && token.text().eq_ignore_ascii_case("SCHEMA")
        )
    })
}

fn classify_other(text: &str) -> HighlightKind {
    if text
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit())
    {
        HighlightKind::NumberLiteral
    } else if is_operator(text) {
        HighlightKind::Operator
    } else {
        HighlightKind::Punctuation
    }
}

/// Classifies one qualified name in the CST.
///
/// The final component receives the pending role; earlier components are
/// namespaces. Without a pending role, a multi-part name classifies its
/// final component as a variable and earlier components as namespaces.
fn classify_qualified_name(
    node: &SyntaxNode<OrnaLanguage>,
    role: Option<NameRole>,
    tokens: &mut Vec<HighlightToken>,
) {
    // Collect the name parts and dots in source order. The name index is
    // recorded so the final component can receive the pending role.
    let mut parts: Vec<(SyntaxToken<OrnaLanguage>, Option<usize>)> = Vec::new();
    let mut name_count = 0usize;
    for child in node.children_with_tokens() {
        if let NodeOrToken::Token(token) = child {
            match token.kind() {
                SyntaxKind::Dot => parts.push((token, None)),
                SyntaxKind::Word | SyntaxKind::QuotedIdentifier => {
                    parts.push((token, Some(name_count)));
                    name_count += 1;
                }
                _ => {}
            }
        }
    }
    let last_index = name_count.saturating_sub(1);
    for (token, name_index) in parts {
        let (start, end) = token_range(&token);
        let kind = match name_index {
            Some(index) if index == last_index => {
                role.map_or(HighlightKind::VariableName, NameRole::kind)
            }
            Some(_) => HighlightKind::NamespaceName,
            None => HighlightKind::Punctuation,
        };
        tokens.push(HighlightToken::new(start, end, kind));
    }
}

/// Classifies the raw token run of one SQL function body.
///
/// SQL bodies are token slices in the CST, so this walker applies SQL-aware
/// heuristics: keywords stay keywords, dotted member references split into
/// namespace and property parts, and the object after `FROM`, `INTO`,
/// `UPDATE`, or `DELETE` is a type name.
fn walk_sql_body(node: &SyntaxNode<OrnaLanguage>, tokens: &mut Vec<HighlightToken>) {
    let children: Vec<NodeOrToken<SyntaxNode<OrnaLanguage>, SyntaxToken<OrnaLanguage>>> =
        node.children_with_tokens().collect();
    let mut pending_table = false;
    let mut after_into = false;
    let mut expect_column_list = false;
    let mut column_list_depth = 0usize;
    let mut pending_set_field = false;
    for (index, child) in children.iter().enumerate() {
        let NodeOrToken::Token(token) = child else {
            walk_node(child.as_node().expect("node child"), &mut None, tokens);
            continue;
        };
        if token.kind() == SyntaxKind::Whitespace {
            continue;
        }
        let (start, end) = token_range(token);
        match token.kind() {
            SyntaxKind::LineComment | SyntaxKind::BlockComment => {
                tokens.push(HighlightToken::new(start, end, HighlightKind::Comment));
            }
            SyntaxKind::StringLiteral => {
                tokens.push(HighlightToken::new(
                    start,
                    end,
                    HighlightKind::StringLiteral,
                ));
            }
            SyntaxKind::QuotedIdentifier => {
                tokens.push(HighlightToken::new(
                    start,
                    end,
                    HighlightKind::QuotedIdentifier,
                ));
            }
            SyntaxKind::Dot | SyntaxKind::Semicolon | SyntaxKind::Comma => {
                tokens.push(HighlightToken::new(start, end, HighlightKind::Punctuation));
            }
            SyntaxKind::LeftParenthesis => {
                if expect_column_list {
                    expect_column_list = false;
                    column_list_depth = 1;
                }
                tokens.push(HighlightToken::new(start, end, HighlightKind::Punctuation));
            }
            SyntaxKind::RightParenthesis => {
                if column_list_depth > 0 {
                    column_list_depth = 0;
                }
                tokens.push(HighlightToken::new(start, end, HighlightKind::Punctuation));
            }
            SyntaxKind::Word => {
                let text = token.text();
                if is_keyword(text) {
                    tokens.push(HighlightToken::new(start, end, HighlightKind::Keyword));
                    if text.eq_ignore_ascii_case("FROM")
                        || text.eq_ignore_ascii_case("UPDATE")
                        || text.eq_ignore_ascii_case("DELETE")
                    {
                        pending_table = true;
                    } else if text.eq_ignore_ascii_case("INTO") {
                        pending_table = true;
                        after_into = true;
                    } else if text.eq_ignore_ascii_case("VALUES") {
                        column_list_depth = 0;
                        expect_column_list = false;
                    } else if text.eq_ignore_ascii_case("SET") {
                        pending_set_field = true;
                    }
                } else if is_scalar_type(text) {
                    tokens.push(HighlightToken::new(start, end, HighlightKind::TypeName));
                } else if column_list_depth > 0 {
                    tokens.push(HighlightToken::new(start, end, HighlightKind::PropertyName));
                } else if pending_set_field {
                    pending_set_field = false;
                    tokens.push(HighlightToken::new(start, end, HighlightKind::PropertyName));
                } else if pending_table {
                    let kind = match next_significant_kind(&children, index + 1) {
                        Some(SyntaxKind::Dot) => HighlightKind::NamespaceName,
                        _ => {
                            pending_table = false;
                            if after_into {
                                expect_column_list = true;
                                after_into = false;
                            }
                            HighlightKind::TypeName
                        }
                    };
                    tokens.push(HighlightToken::new(start, end, kind));
                } else if previous_significant_kind(&children, index) == Some(SyntaxKind::Dot) {
                    tokens.push(HighlightToken::new(start, end, HighlightKind::PropertyName));
                } else if next_significant_kind(&children, index + 1) == Some(SyntaxKind::Dot) {
                    tokens.push(HighlightToken::new(
                        start,
                        end,
                        HighlightKind::NamespaceName,
                    ));
                } else {
                    tokens.push(HighlightToken::new(start, end, HighlightKind::VariableName));
                }
            }
            SyntaxKind::Other => {
                tokens.push(HighlightToken::new(
                    start,
                    end,
                    classify_other(token.text()),
                ));
            }
            SyntaxKind::Whitespace => {}
            // The lexer never emits node kinds as tokens.
            _ => {}
        }
    }
}

fn previous_significant_kind(
    children: &[NodeOrToken<SyntaxNode<OrnaLanguage>, SyntaxToken<OrnaLanguage>>],
    index: usize,
) -> Option<SyntaxKind> {
    children[..index]
        .iter()
        .rev()
        .find_map(|child| match child {
            NodeOrToken::Token(token) if token.kind() == SyntaxKind::Whitespace => None,
            NodeOrToken::Token(token) => Some(token.kind()),
            NodeOrToken::Node(node) => Some(node.kind()),
        })
}

fn next_significant_kind(
    children: &[NodeOrToken<SyntaxNode<OrnaLanguage>, SyntaxToken<OrnaLanguage>>],
    start: usize,
) -> Option<SyntaxKind> {
    children[start..].iter().find_map(|child| match child {
        NodeOrToken::Token(token) if token.kind() == SyntaxKind::Whitespace => None,
        NodeOrToken::Token(token) => Some(token.kind()),
        NodeOrToken::Node(node) => Some(node.kind()),
    })
}

fn token_range(token: &SyntaxToken<OrnaLanguage>) -> (usize, usize) {
    let range = token.text_range();
    (usize::from(range.start()), usize::from(range.end()))
}

/// Returns true when the word is an Orna or SQL keyword.
fn is_keyword(word: &str) -> bool {
    let upper = word.to_ascii_uppercase();
    KEYWORDS
        .binary_search_by(|candidate| (*candidate).cmp(upper.as_str()))
        .is_ok()
}

/// Returns true when the word is a standard scalar type name.
fn is_scalar_type(word: &str) -> bool {
    let upper = word.to_ascii_uppercase();
    SCALAR_TYPES
        .binary_search_by(|candidate| (*candidate).cmp(upper.as_str()))
        .is_ok()
}

fn is_operator(text: &str) -> bool {
    matches!(
        text,
        ":=" | "=>"
            | "="
            | "<>"
            | "!="
            | "<"
            | ">"
            | "<="
            | ">="
            | "+"
            | "-"
            | "*"
            | "/"
            | "%"
            | "||"
            | "->"
            | ":"
            | "?"
    )
}

/// Orna and SQL keywords recognised by the classifier, sorted for binary search.
pub const KEYWORDS: &[&str] = &[
    "ADD",
    "ALL",
    "ALTER",
    "AND",
    "AS",
    "ASC",
    "ATOMIC",
    "AWAIT",
    "BEGIN",
    "BETWEEN",
    "BY",
    "CALL",
    "CAPABILITY",
    "CASCADE",
    "CASE",
    "CHECK",
    "CLIENT",
    "CONST",
    "CONTRACT",
    "CREATE",
    "CROSS",
    "DEFAULT",
    "DEFINER",
    "DELETE",
    "DESC",
    "DISABLED",
    "DISTINCT",
    "DOCUMENTATION",
    "DROP",
    "ELSE",
    "ELSIF",
    "END",
    "ENUM",
    "EXECUTE",
    "EXISTS",
    "EXPORT",
    "EXTERNAL",
    "FALSE",
    "FIELD",
    "FINAL",
    "FIRST",
    "FOR",
    "FROM",
    "FULL",
    "FUNCTION",
    "GRANT",
    "GROUP",
    "HAVING",
    "IF",
    "ILIKE",
    "IMMUTABLE",
    "IN",
    "INNER",
    "INSERT",
    "INSPECT",
    "INTO",
    "INVOKER",
    "IS",
    "JOIN",
    "KERNEL",
    "LAST",
    "LEFT",
    "LET",
    "LIKE",
    "LIMIT",
    "LIST",
    "LOCAL",
    "LOOP",
    "MANUAL",
    "MAP",
    "NOT",
    "NULL",
    "NULLS",
    "OBJECT",
    "OFFSET",
    "ON",
    "ONLY",
    "OPAQUE",
    "OPTION",
    "OR",
    "ORDER",
    "OUTER",
    "PERSISTABLE",
    "PRIMITIVE",
    "READ",
    "REF",
    "RENAME",
    "REQUIRES",
    "RESTRICT",
    "RETURN",
    "RETURNING",
    "RETURNS",
    "REVOKE",
    "RIGHT",
    "ROLE",
    "ROWS",
    "RUNTIME",
    "SCHEMA",
    "SCOPE",
    "SEALED",
    "SECURITY",
    "SELECT",
    "SERVER",
    "SESSION",
    "SET",
    "STABLE",
    "STATE",
    "STREAM",
    "TABLE",
    "THEN",
    "TO",
    "TRANSACTION",
    "TRANSIENT",
    "TRUE",
    "TYPE",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USER",
    "VALUE",
    "VALUES",
    "VOLATILE",
    "VOLATILITY",
    "WHEN",
    "WHERE",
    "WHILE",
];

/// Standard scalar type names, sorted for binary search.
pub const SCALAR_TYPES: &[&str] = &[
    "BIGINT",
    "BINARY LARGE OBJECT",
    "BOOL",
    "BOOLEAN",
    "BYTES",
    "CHARACTER LARGE OBJECT",
    "DATE",
    "DECIMAL",
    "DURATION",
    "FLOAT",
    "INT",
    "INTEGER",
    "TEXT",
    "TIME",
    "TIMESTAMP",
    "UUID",
    "VOID",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<(Range<usize>, HighlightKind)> {
        highlight(source)
            .into_iter()
            .map(|token| (token.range, token.kind))
            .collect()
    }

    fn kind_at(source: &str, needle: &str) -> HighlightKind {
        kinds(source)
            .into_iter()
            .find(|(range, _)| source.get(range.clone()) == Some(needle))
            .unwrap_or_else(|| panic!("no token matching {needle:?}"))
            .1
    }

    #[test]
    fn classifies_declaration_keywords() {
        let source = "CREATE SCHEMA tasks;";
        assert_eq!(kind_at(source, "CREATE"), HighlightKind::Keyword);
        assert_eq!(kind_at(source, "SCHEMA"), HighlightKind::Keyword);
    }

    #[test]
    fn classifies_schema_name_as_namespace() {
        let source = "CREATE SCHEMA tasks;";
        assert_eq!(kind_at(source, "tasks"), HighlightKind::NamespaceName);
    }

    #[test]
    fn classifies_object_type_declaration() {
        let source = "CREATE TYPE crm.customer AS OBJECT (\n  title TEXT NOT NULL\n);";
        assert_eq!(kind_at(source, "TYPE"), HighlightKind::Keyword);
        assert_eq!(kind_at(source, "OBJECT"), HighlightKind::Keyword);
        assert_eq!(kind_at(source, "crm"), HighlightKind::NamespaceName);
        assert_eq!(kind_at(source, "customer"), HighlightKind::TypeName);
        assert_eq!(kind_at(source, "title"), HighlightKind::PropertyName);
        assert_eq!(kind_at(source, "TEXT"), HighlightKind::TypeName);
        assert_eq!(kind_at(source, "NOT"), HighlightKind::Keyword);
        assert_eq!(kind_at(source, "NULL"), HighlightKind::Keyword);
    }

    #[test]
    fn classifies_enum_and_value_types() {
        let source = "CREATE TYPE tasks.status AS ENUM ('open', 'closed');";
        assert_eq!(kind_at(source, "status"), HighlightKind::TypeName);
        assert_eq!(kind_at(source, "ENUM"), HighlightKind::Keyword);
        assert_eq!(kind_at(source, "'open'"), HighlightKind::StringLiteral);

        let record =
            "CREATE TYPE tasks.money AS VALUE ( amount DECIMAL, currency TEXT ) IMMUTABLE;";
        assert_eq!(kind_at(record, "money"), HighlightKind::TypeName);
        assert_eq!(kind_at(record, "VALUE"), HighlightKind::Keyword);
        assert_eq!(kind_at(record, "amount"), HighlightKind::PropertyName);
        assert_eq!(kind_at(record, "IMMUTABLE"), HighlightKind::Keyword);

        let opaque = "CREATE TYPE std.io.ByteStream AS VALUE OPAQUE KERNEL CONTRACT 'x' TRANSIENT;";
        assert_eq!(kind_at(opaque, "OPAQUE"), HighlightKind::Keyword);
        assert_eq!(kind_at(opaque, "KERNEL"), HighlightKind::Keyword);
        assert_eq!(kind_at(opaque, "CONTRACT"), HighlightKind::Keyword);
        assert_eq!(kind_at(opaque, "TRANSIENT"), HighlightKind::Keyword);
        assert_eq!(kind_at(opaque, "'x'"), HighlightKind::StringLiteral);
    }

    #[test]
    fn classifies_canonical_scalar_type_spellings() {
        let source = "CREATE SERVER FUNCTION files.boolean_value() RETURNS BOOLEAN AS SELECT TRUE;
            CREATE SERVER FUNCTION files.integer_value() RETURNS INTEGER AS SELECT TRUE;
            CREATE SERVER FUNCTION files.text_value() RETURNS CHARACTER LARGE OBJECT AS SELECT TRUE;
            CREATE SERVER FUNCTION files.bytes_value() RETURNS BINARY LARGE OBJECT AS SELECT TRUE;";

        for needle in [
            "BOOLEAN",
            "INTEGER",
            "CHARACTER",
            "LARGE",
            "OBJECT",
            "BINARY",
        ] {
            assert_eq!(kind_at(source, needle), HighlightKind::TypeName, "{needle}");
        }
    }

    #[test]
    fn classifies_stream_return_element_type() {
        let source = "CREATE SERVER FUNCTION tasks.events() RETURNS STREAM<tasks.event> AS SELECT REF(e) FROM tasks.event e;";
        assert_eq!(kind_at(source, "STREAM"), HighlightKind::Keyword);
        assert_eq!(kind_at(source, "tasks"), HighlightKind::NamespaceName);
        assert_eq!(kind_at(source, "event"), HighlightKind::TypeName);
    }

    #[test]
    fn classifies_server_function_declaration() {
        let source = "CREATE SERVER FUNCTION tasks.overdue ( p_before TIMESTAMP )\nRETURNS ROWS ( title TEXT )\nAS\n  SELECT t.title FROM tasks.task t WHERE t.due_at < p_before;";
        assert_eq!(kind_at(source, "SERVER"), HighlightKind::Keyword);
        assert_eq!(kind_at(source, "FUNCTION"), HighlightKind::Keyword);
        assert_eq!(kind_at(source, "overdue"), HighlightKind::FunctionName);
        assert_eq!(kind_at(source, "p_before"), HighlightKind::VariableName);
        assert_eq!(kind_at(source, "TIMESTAMP"), HighlightKind::TypeName);
        assert_eq!(kind_at(source, "RETURNS"), HighlightKind::Keyword);
        assert_eq!(kind_at(source, "ROWS"), HighlightKind::Keyword);
        assert_eq!(kind_at(source, "title"), HighlightKind::PropertyName);
        assert_eq!(kind_at(source, "AS"), HighlightKind::Keyword);
    }

    #[test]
    fn classifies_client_function_with_ui_type() {
        let source = "CREATE CLIENT FUNCTION studio.main ()\nRETURNS std.ui.UI\nAS\n  std.ui.window(title => 'Studio');";
        assert_eq!(kind_at(source, "CLIENT"), HighlightKind::Keyword);
        assert_eq!(kind_at(source, "main"), HighlightKind::FunctionName);
        assert_eq!(kind_at(source, "std"), HighlightKind::NamespaceName);
        assert_eq!(kind_at(source, "ui"), HighlightKind::NamespaceName);
        assert_eq!(kind_at(source, "UI"), HighlightKind::TypeName);
        assert_eq!(kind_at(source, "window"), HighlightKind::FunctionName);
        assert_eq!(kind_at(source, "title"), HighlightKind::VariableName);
        assert_eq!(kind_at(source, "=>"), HighlightKind::Operator);
        assert_eq!(kind_at(source, "'Studio'"), HighlightKind::StringLiteral);
    }

    #[test]
    fn classifies_sql_query_body() {
        let source = "CREATE SERVER FUNCTION f ()\nRETURNS BOOL\nAS\n  SELECT c.completed\n    FROM crm.customer c\n   WHERE c.due_at < p_before\n   ORDER BY c.due_at;";
        assert_eq!(kind_at(source, "SELECT"), HighlightKind::Keyword);
        assert_eq!(kind_at(source, "completed"), HighlightKind::PropertyName);
        assert_eq!(kind_at(source, "FROM"), HighlightKind::Keyword);
        assert_eq!(kind_at(source, "crm"), HighlightKind::NamespaceName);
        assert_eq!(kind_at(source, "customer"), HighlightKind::TypeName);
        assert_eq!(kind_at(source, "c"), HighlightKind::NamespaceName);
        assert_eq!(kind_at(source, "WHERE"), HighlightKind::Keyword);
        assert_eq!(kind_at(source, "due_at"), HighlightKind::PropertyName);
        assert_eq!(kind_at(source, "ORDER"), HighlightKind::Keyword);
        assert_eq!(kind_at(source, "BY"), HighlightKind::Keyword);
        assert_eq!(kind_at(source, "<"), HighlightKind::Operator);
    }

    #[test]
    fn classifies_mutation_bodies() {
        let insert = "CREATE SERVER FUNCTION f ()\nRETURNS VOID\nAS\n  INSERT INTO crm.customer (name) VALUES ('x');";
        assert_eq!(kind_at(insert, "INSERT"), HighlightKind::Keyword);
        assert_eq!(kind_at(insert, "INTO"), HighlightKind::Keyword);
        assert_eq!(kind_at(insert, "customer"), HighlightKind::TypeName);
        assert_eq!(kind_at(insert, "name"), HighlightKind::PropertyName);
        assert_eq!(kind_at(insert, "VALUES"), HighlightKind::Keyword);
        assert_eq!(kind_at(insert, "'x'"), HighlightKind::StringLiteral);

        let update = "CREATE SERVER FUNCTION f ()\nRETURNS VOID\nAS\n  UPDATE crm.customer c SET completed = TRUE WHERE REF(c) = p_key;";
        assert_eq!(kind_at(update, "UPDATE"), HighlightKind::Keyword);
        assert_eq!(kind_at(update, "customer"), HighlightKind::TypeName);
        assert_eq!(kind_at(update, "SET"), HighlightKind::Keyword);
        assert_eq!(kind_at(update, "completed"), HighlightKind::PropertyName);
        assert_eq!(kind_at(update, "="), HighlightKind::Operator);
        assert_eq!(kind_at(update, "TRUE"), HighlightKind::Keyword);

        let delete = "CREATE SERVER FUNCTION f ()\nRETURNS VOID\nAS\n  DELETE FROM crm.customer c WHERE REF(c) = p_key;";
        assert_eq!(kind_at(delete, "DELETE"), HighlightKind::Keyword);
        assert_eq!(kind_at(delete, "customer"), HighlightKind::TypeName);
    }

    #[test]
    fn classifies_comments_and_numbers() {
        let source = "-- a comment\nCREATE TYPE x AS VALUE ( n INT ); /* block */\nSELECT 42;";
        assert_eq!(kind_at(source, "-- a comment"), HighlightKind::Comment);
        assert_eq!(kind_at(source, "/* block */"), HighlightKind::Comment);
        assert_eq!(kind_at(source, "42"), HighlightKind::NumberLiteral);
    }

    #[test]
    fn classifies_reference_and_collection_types() {
        let source = "CREATE TYPE t AS OBJECT ( assignee REF tasks.task, tags LIST<TEXT> );";
        assert_eq!(kind_at(source, "REF"), HighlightKind::Keyword);
        assert_eq!(kind_at(source, "assignee"), HighlightKind::PropertyName);
        assert_eq!(kind_at(source, "LIST"), HighlightKind::Keyword);
        assert_eq!(kind_at(source, "<"), HighlightKind::Operator);
        assert_eq!(kind_at(source, ">"), HighlightKind::Operator);
    }

    #[test]
    fn classifies_quoted_identifiers_and_alter() {
        let source =
            "CREATE SCHEMA \"My Schema\";\nALTER TYPE tasks.task RENAME FIELD title TO heading;";
        assert_eq!(
            kind_at(source, "\"My Schema\""),
            HighlightKind::NamespaceName
        );
        assert_eq!(kind_at(source, "ALTER"), HighlightKind::Keyword);
        assert_eq!(kind_at(source, "RENAME"), HighlightKind::Keyword);
        assert_eq!(kind_at(source, "title"), HighlightKind::PropertyName);
        assert_eq!(kind_at(source, "heading"), HighlightKind::PropertyName);
        assert_eq!(kind_at(source, "tasks"), HighlightKind::NamespaceName);
        assert_eq!(kind_at(source, "task"), HighlightKind::TypeName);
    }

    #[test]
    fn classifies_grants_and_roles() {
        let source = "CREATE ROLE developer;\nGRANT EXECUTE ON FUNCTION studio.main TO developer;";
        assert_eq!(kind_at(source, "ROLE"), HighlightKind::Keyword);
        assert_eq!(kind_at(source, "GRANT"), HighlightKind::Keyword);
        assert_eq!(kind_at(source, "EXECUTE"), HighlightKind::Keyword);
        assert_eq!(kind_at(source, "ON"), HighlightKind::Keyword);
        assert_eq!(kind_at(source, "main"), HighlightKind::FunctionName);
        assert_eq!(kind_at(source, "developer"), HighlightKind::VariableName);
    }

    #[test]
    fn covers_every_non_whitespace_byte() {
        let source = "CREATE TYPE tasks.task AS OBJECT ( title TEXT NOT NULL );\nCREATE SERVER FUNCTION tasks.f () RETURNS BOOL AS SELECT TRUE;\n-- tail\n";
        let tokens = highlight(source);
        let mut covered = vec![false; source.len()];
        for token in &tokens {
            for byte in token.range.clone() {
                assert!(!covered[byte], "overlapping token {token:?}");
                covered[byte] = true;
            }
        }
        for (index, character) in source.char_indices() {
            if !character.is_whitespace() {
                assert!(covered[index], "uncovered byte {index}");
            }
        }
        assert!(tokens.iter().any(|t| t.kind == HighlightKind::Comment));
    }

    #[test]
    fn tolerates_partial_source() {
        let partial = "CREATE TYPE tasks.task AS OBJECT ( title ";
        let tokens = highlight(partial);
        assert!(!tokens.is_empty());
        assert!(tokens.iter().any(|t| t.kind == HighlightKind::Keyword));
        assert!(tokens.iter().any(|t| t.kind == HighlightKind::PropertyName));

        let gibberish = ")))(((( ::: ???";
        let tokens = highlight(gibberish);
        assert!(
            tokens
                .iter()
                .all(|t| t.kind != HighlightKind::StringLiteral)
        );
    }

    #[test]
    fn classifies_procedural_keywords_in_opaque_bodies() {
        let source = "CREATE SERVER FUNCTION f ()\nRETURNS VOID\nIS\n  LET v TIMESTAMP := sys.clock.now();\nBEGIN\n  RETURN v;\nEND;";
        for needle in ["IS", "LET", "BEGIN", "RETURN", "END"] {
            assert_eq!(kind_at(source, needle), HighlightKind::Keyword, "{needle}");
        }
        assert_eq!(kind_at(source, "v"), HighlightKind::VariableName);
        assert_eq!(kind_at(source, ":="), HighlightKind::Operator);
        assert_eq!(kind_at(source, "sys"), HighlightKind::NamespaceName);
        assert_eq!(kind_at(source, "clock"), HighlightKind::PropertyName);
        assert_eq!(kind_at(source, "now"), HighlightKind::FunctionName);
    }
}
