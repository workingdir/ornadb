//! Doc comments and interface clauses reach the definition they annotate.
//!
//! The parser attaches `///` lines and interface clauses to each item. Lowering
//! copies them onto the [`orna_hir::Def`], so tools read a declaration's
//! purpose text and lifecycle facts from the HIR rather than from the source.

use orna_hir::{DefKind, ModulePath, Name, Program, SourceId, SourceUnit, lower};
use orna_syntax::{parse_interface, parse_module_with_file};

fn trimmed(lines: &[String]) -> Vec<&str> {
    lines.iter().map(|line| line.trim()).collect()
}

fn def_named<'a>(program: &'a Program, name: &str) -> &'a orna_hir::Def {
    program
        .defs
        .iter()
        .find(|def| def.name.as_str() == name)
        .unwrap_or_else(|| panic!("definition `{name}` exists"))
}

#[test]
fn user_declaration_carries_its_doc_comment() {
    let parsed = parse_module_with_file(
        "/// Adds two numbers.\npub fn add(a: Int, b: Int): Int { a + b }\n",
        "user.orna",
    );
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    let path = ModulePath::new([Name::new("main")]);
    let program = lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &parsed.value,
        path,
    }]);

    let def = def_named(&program, "add");
    assert!(matches!(def.kind, DefKind::Fn(_)));
    assert_eq!(trimmed(&def.docs), vec!["Adds two numbers."]);
    assert_eq!(def.clauses, orna_syntax::InterfaceClauses::default());
}

#[test]
fn engine_declaration_carries_docs_and_clauses() {
    let source = "\
/// Looks up a stable object.
///
/// # Invariants
///
/// - does not grant authority
pub native fn probe(id: Str): Str effect table_read since \"1.1.0\" fails object__not_found;
";
    let parsed = parse_interface(source);
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
    let path = ModulePath::new([Name::new("sys"), Name::new("probe_test")]);
    let program = lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &parsed.value,
        path,
    }]);

    let def = def_named(&program, "probe");
    assert!(matches!(def.kind, DefKind::Fn(_)));
    assert_eq!(
        trimmed(&def.docs),
        vec![
            "Looks up a stable object.",
            "",
            "# Invariants",
            "",
            "- does not grant authority"
        ]
    );
    assert_eq!(
        def.clauses.since.as_ref().map(|since| since.text.as_str()),
        Some("1.1.0")
    );
    let fails: Vec<&str> = def
        .clauses
        .fails
        .iter()
        .map(|code| code.text.as_str())
        .collect();
    assert_eq!(fails, vec!["object__not_found"]);
}
