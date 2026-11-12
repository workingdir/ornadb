//! `use module as _;` binds exactly the prelude names the caller records (ORNA-IMPORT-005).
//!
//! The HIR never reads the standard profile; these tests state the prelude
//! directly and assert what lowering binds for the importing module.

use orna_hir::{
    ModulePath, Name, PreludeExports, Program, SourceId, SourceUnit, diag, lower,
    lower_with_prelude,
};
use orna_syntax::{SyntaxTree, parse_module_with_file};

const LIB: &str = "pub fn increment(x: Int): Int = x + 1;\nfn hidden(x: Int): Int = x;\n";

fn parse(source: &str) -> SyntaxTree {
    let parsed = parse_module_with_file(source, "test.orna");
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    parsed.value
}

fn path(segment: &str) -> ModulePath {
    ModulePath::new([Name::new(segment)])
}

/// Lowers the `lib` and `client` sources, the second with the given prelude.
fn lower_client(client: &str, prelude: Option<&PreludeExports>) -> Program {
    let lib = parse(LIB);
    let client_tree = parse(client);
    let units = [
        SourceUnit {
            source: SourceId::new(0),
            tree: &lib,
            path: path("lib"),
        },
        SourceUnit {
            source: SourceId::new(1),
            tree: &client_tree,
            path: path("client"),
        },
    ];
    let program = match prelude {
        Some(prelude) => lower_with_prelude(&units, prelude),
        None => lower(&units),
    };
    assert_eq!(program.validate(), Ok(()), "lowered program must validate");
    program
}

/// The `(code, message)` of every diagnostic raised in the `client` module.
fn client_diagnostics(program: &Program) -> Vec<(&'static str, String)> {
    program
        .diagnostics()
        .filter(|(module, _)| program.modules[*module].path.to_string() == "client")
        .map(|(_, diagnostic)| (diagnostic.code, diagnostic.message.to_string()))
        .collect()
}

fn lib_prelude(names: &[&str]) -> PreludeExports {
    PreludeExports::default().with_module(path("lib"), names.iter().copied())
}

#[test]
fn recorded_prelude_name_binds_for_the_importing_module() {
    let prelude = lib_prelude(&["increment"]);
    let program = lower_client(
        "use lib as _;\npub fn run(): Int = increment(41);\n",
        Some(&prelude),
    );
    assert_eq!(client_diagnostics(&program), Vec::new());
}

#[test]
fn without_a_prelude_the_import_binds_nothing() {
    let program = lower_client("use lib as _;\npub fn run(): Int = increment(41);\n", None);
    let diagnostics = client_diagnostics(&program);
    assert!(
        diagnostics
            .iter()
            .any(|(code, message)| *code == diag::UNRESOLVED
                && message == "`increment` is not in scope"),
        "{diagnostics:?}"
    );
}

#[test]
fn a_module_without_a_recorded_prelude_imports_no_names() {
    let prelude = lib_prelude(&["increment"]).with_module(path("other"), ["increment"]);
    let program = lower_client(
        "use lib as _;\npub fn run(): Int = increment(41);\n",
        Some(&prelude),
    );
    assert_eq!(client_diagnostics(&program), Vec::new());

    let unrecorded = PreludeExports::default().with_module(path("other"), ["increment"]);
    let program = lower_client(
        "use lib as _;\npub fn run(): Int = increment(41);\n",
        Some(&unrecorded),
    );
    let diagnostics = client_diagnostics(&program);
    assert!(
        diagnostics
            .iter()
            .any(|(code, message)| *code == diag::UNRESOLVED
                && message == "`increment` is not in scope"),
        "{diagnostics:?}"
    );
}

#[test]
fn a_prelude_name_that_is_not_public_is_rejected() {
    let prelude = lib_prelude(&["hidden"]);
    let program = lower_client("use lib as _;\n", Some(&prelude));
    let diagnostics = client_diagnostics(&program);
    assert!(
        diagnostics.iter().any(|(code, message)| {
            *code == diag::IMPORT && message == "prelude export `hidden` is not public"
        }),
        "{diagnostics:?}"
    );
}

#[test]
fn a_prelude_name_the_module_does_not_declare_is_rejected() {
    let prelude = lib_prelude(&["missing"]);
    let program = lower_client("use lib as _;\n", Some(&prelude));
    let diagnostics = client_diagnostics(&program);
    assert!(
        diagnostics.iter().any(|(code, message)| {
            *code == diag::IMPORT && message == "prelude export `missing` is not public"
        }),
        "{diagnostics:?}"
    );
}
