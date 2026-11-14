//! Labelled arguments on root forms whose parameters the spec names.
//!
//! The spec writes `error(code: .., message: ..)` and `bucket_by(values, period, zone)`, so a
//! label on those forms binds to the declared parameter. A label the form does not declare is
//! still an argument diagnostic, and a call that omits a required parameter is too.

use orna_hir::typeck::typeck;
use orna_hir::{ModulePath, Name, Program, SourceId, SourceUnit, lower};
use orna_syntax::parse_module_with_file;
use orna_syntax_ast::SyntaxTree;

fn lower_source(source: &str) -> Program {
    let parsed = parse_module_with_file(source, "test.orna");
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    let tree: SyntaxTree = parsed.value;
    let path = ModulePath::new([Name::new("main")]);
    let program = lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &tree,
        path,
    }]);
    assert_eq!(program.validate(), Ok(()), "lowered program must validate");
    program
}

/// The argument diagnostic messages the `main` module carries.
fn argument_messages(program: &Program) -> Vec<String> {
    program
        .modules
        .iter()
        .filter(|module| module.path.to_string() == "main")
        .flat_map(|module| module.diagnostics.iter())
        .filter(|diagnostic| diagnostic.code == orna_hir::diag::ARGUMENT)
        .map(|diagnostic| diagnostic.message.to_string())
        .collect()
}

#[test]
fn error_accepts_its_declared_labels() {
    let program = lower_source(
        "pub fn failure(): Str = error(code: \"email.invalid\", message: \"invalid email\");\n",
    );
    assert_eq!(argument_messages(&program), Vec::<String>::new());
}

#[test]
fn error_rejects_an_undeclared_label() {
    let program = lower_source(
        "pub fn failure(): Str = error(code: \"x\", message: \"y\", detail: \"z\");\n",
    );
    assert_eq!(
        argument_messages(&program),
        vec!["no parameter is named `detail`".to_string()]
    );
}

#[test]
fn error_still_requires_its_message() {
    let program = lower_source("pub fn failure(): Str = error(code: \"x\");\n");
    assert_eq!(
        argument_messages(&program),
        vec!["the call omits a required parameter".to_string()]
    );
}

#[test]
fn bucket_by_accepts_a_named_zone() {
    let program = lower_source(
        "pub fn bucketed(values: Str): Str = values | bucket_by(1.day, zone: Europe.London);\n",
    );
    assert_eq!(argument_messages(&program), Vec::<String>::new());
}

#[test]
fn bucket_by_accepts_an_omitted_zone() {
    let program = lower_source("pub fn bucketed(values: Str): Str = values | bucket_by(1.day);\n");
    assert_eq!(argument_messages(&program), Vec::<String>::new());
}

#[test]
fn bucket_by_rejects_an_undeclared_label() {
    let program = lower_source(
        "pub fn bucketed(values: Str): Str = values | bucket_by(1.day, timezone: Europe.London);\n",
    );
    assert_eq!(
        argument_messages(&program),
        vec!["no parameter is named `timezone`".to_string()]
    );
}

/// Lowers and types `source`, then returns the argument diagnostics of the `main` module.
fn typed_argument_messages(source: &str) -> Vec<String> {
    let mut program = lower_source(source);
    typeck(&mut program);
    argument_messages(&program)
}

const GREETER: &str = "pub protocol Greeter { fn greet(self, punctuation: Str): Str; }\n\
    pub enum Rounding {\n\
        half_up,\n\
        impl Greeter { fn greet(self, punctuation: Str): Str = \"hi\"; }\n\
    }\n";

#[test]
fn a_protocol_member_accepts_its_declared_label() {
    let source = format!("{GREETER}pub fn f(r: Rounding): Str = r.greet(punctuation: \"!\");\n");
    assert_eq!(typed_argument_messages(&source), Vec::<String>::new());
}

#[test]
fn a_protocol_member_rejects_an_undeclared_label() {
    let source = format!("{GREETER}pub fn f(r: Rounding): Str = r.greet(bogus: \"!\");\n");
    let messages = typed_argument_messages(&source);
    assert!(
        messages.iter().any(|m| m.contains("no parameter is named `bogus`")),
        "{messages:?}"
    );
}

#[test]
fn a_protocol_member_rejects_an_omitted_required_label() {
    let source = format!("{GREETER}pub fn f(r: Rounding): Str = r.greet(bogus: \"!\");\n");
    let messages = typed_argument_messages(&source);
    assert!(
        messages.iter().any(|m| m.contains("omits a required parameter")),
        "{messages:?}"
    );
}

#[test]
fn a_protocol_member_rejects_a_repeated_label() {
    let source = format!(
        "{GREETER}pub fn f(r: Rounding): Str = r.greet(punctuation: \"!\", punctuation: \"?\");\n"
    );
    let messages = typed_argument_messages(&source);
    assert!(
        messages.iter().any(|m| m.contains("supplied twice")),
        "{messages:?}"
    );
}
