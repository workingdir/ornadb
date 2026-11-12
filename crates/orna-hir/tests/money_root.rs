//! `Money` and `Currency` are root-environment names (hir-host.md B2): a module reaches them with no `use`.
//! A local declaration of the same name still wins.

use orna_hir::{ModulePath, Name, Program, SourceId, SourceUnit, lower};
use orna_syntax::{SyntaxTree, parse_module_with_file};

fn parse(source: &str) -> SyntaxTree {
    let parsed = parse_module_with_file(source, "money.orna");
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    parsed.value
}

/// The diagnostics of the user module `main`, as code and message.
fn user_diagnostics(program: &Program) -> Vec<String> {
    program
        .modules
        .iter()
        .filter(|module| module.path == ModulePath::new([Name::new("main")]))
        .flat_map(|module| {
            module
                .diagnostics
                .iter()
                .map(|diagnostic| format!("{} {}", diagnostic.code, diagnostic.message))
        })
        .collect()
}

fn lower_main(source: &str) -> Program {
    let tree = parse(source);
    lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &tree,
        path: ModulePath::new([Name::new("main")]),
    }])
}

const GBP: &str = "pub type GBP {\n    impl Currency {\n        static code = \"GBP\";\n        static minor_digits = 2;\n    }\n}\n";

#[test]
fn money_resolves_with_no_use() {
    let program = lower_main(&format!(
        "{GBP}pub fn wrap(amount: Money<GBP>): Money<GBP> = amount;\n"
    ));
    assert_eq!(user_diagnostics(&program), Vec::<String>::new());
}

#[test]
fn the_diagnostic_reader_sees_an_unresolved_name() {
    let program = lower_main("pub fn wrap(amount: Nope): Nope = amount;\n");
    assert!(!user_diagnostics(&program).is_empty());
}
#[test]
fn a_local_money_declaration_wins_over_the_root_name() {
    let program = lower_main(
        "pub type Money {\n    cents: Int,\n}\npub fn wrap(amount: Money): Money = amount;\n",
    );
    assert_eq!(user_diagnostics(&program), Vec::<String>::new());
}
