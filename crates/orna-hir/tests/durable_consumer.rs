//! The durable consumer rule (E9101, ORNA-CONSUMER-001): one source named twice in one
//! `parallel([...])` list shares a consumer identity. The rule runs during lowering.

use orna_hir::{ModulePath, Name, Program, SourceId, SourceUnit, lower};
use orna_syntax::parse_module_with_file;

fn lowered(source: &str) -> Program {
    let parsed = parse_module_with_file(source, "test.orna");
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    let tree = parsed.value;
    let units = [SourceUnit {
        source: SourceId::new(0),
        tree: &tree,
        path: ModulePath::new([Name::new("main")]),
    }];
    lower(&units)
}

fn consumer_diagnostics(program: &Program) -> usize {
    program
        .diagnostics()
        .filter(|(_, diagnostic)| diagnostic.code == "E9101")
        .count()
}

#[test]
fn a_repeated_qualified_source_is_one_duplicate_consumer() {
    let program = lowered("pub fn main() = parallel([mail.google.sync, mail.google.sync]);\n");
    assert_eq!(consumer_diagnostics(&program), 1);
}

#[test]
fn distinct_qualified_sources_are_not_duplicates() {
    let program = lowered("pub fn main() = parallel([mail.google.sync, mail.google.inbox]);\n");
    assert_eq!(consumer_diagnostics(&program), 0);
}

#[test]
fn a_repeated_defined_function_is_one_duplicate_consumer() {
    let program = lowered("pub fn sync() = 1;\npub fn main() = parallel([sync, sync]);\n");
    assert_eq!(consumer_diagnostics(&program), 1);
}

#[test]
fn a_repeat_outside_parallel_is_not_a_consumer_duplicate() {
    let program = lowered("pub fn main() = [mail.google.sync, mail.google.sync];\n");
    assert_eq!(consumer_diagnostics(&program), 0);
}

#[test]
fn one_call_reports_its_first_repeat_once() {
    let program = lowered(
        "pub fn main() = parallel([mail.google.sync, mail.google.sync, mail.google.sync]);\n",
    );
    assert_eq!(consumer_diagnostics(&program), 1);
}
