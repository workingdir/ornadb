//! The REPL session's module form: the definitions a session accumulates, where a top-level `let`
//! is admitted as an item. A module unit still rejects a top-level `let`.

use crate::parser::{Parse, annotate_tree, located, parse_items};

use orna_syntax_ast::SyntaxTree;

/// Parses one session module: definitions plus top-level `let` bindings, located in `file`.
pub fn parse_session_module_with_file(source: &str, file: impl Into<String>) -> Parse<SyntaxTree> {
    let file = file.into();
    located(
        parse_items(source, false, false),
        source,
        &file,
        annotate_tree,
    )
}

#[cfg(test)]
mod tests {
    use super::parse_session_module_with_file;
    use crate::parse_module_with_file;
    use orna_syntax_ast::ItemKind;

    #[test]
    fn a_top_level_let_is_an_item_of_a_session_module() {
        let parsed = parse_session_module_with_file("let seed: Int = 10;", "repl.orna");
        assert!(
            parsed.is_ok(),
            "a session let parses: {:?}",
            parsed.diagnostics
        );
        assert_eq!(parsed.value.items.len(), 1);
        assert_eq!(parsed.value.items[0].kind, ItemKind::Let);
    }

    #[test]
    fn definitions_and_lets_share_one_session_module() {
        let source = "fn double(x: Int): Int = x * 2;\nlet seed = 10;";
        let parsed = parse_session_module_with_file(source, "repl.orna");
        assert!(parsed.is_ok(), "both forms parse: {:?}", parsed.diagnostics);
        let kinds: Vec<ItemKind> = parsed.value.items.iter().map(|item| item.kind).collect();
        assert_eq!(kinds, [ItemKind::Function, ItemKind::Let]);
    }

    #[test]
    fn a_module_unit_still_rejects_a_top_level_let() {
        let parsed = parse_module_with_file("let seed = 10;", "main.orna");
        assert!(!parsed.is_ok(), "a module unit admits no top-level let");
    }
}
