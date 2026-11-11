//! String interpolation (kernel-and-library.md 4.5, P5): each `{expr}` runs the `Display` instance of the expression's
//! type. The engine renders the scalar kernel types natively, and a user type runs its own `impl Display` through
//! protocol dispatch.

use orna_exec::{ExecError, ExecLimits, Value, run_entry};
use orna_hir::{DefId, DefKind, ModulePath, Name, Program, SourceId, SourceUnit, lower, typeck};
use orna_syntax::parse_module_with_file;

const LIMITS: ExecLimits = ExecLimits {
    max_steps: 100_000,
    max_depth: 64,
    max_collection_items: 2_048,
    max_string_bytes: 1 << 20,
    max_integer_digits: 1_024,
};

const CONTACT: &str = "type Contact {\n    pub name: Str,\n    impl Display {\n        fn display(self): Str = self.name;\n    }\n}\npub fn contact(): Contact = Contact { name: \"ada\" };\n";

fn lower_typed(source: &str) -> Program {
    let parsed = parse_module_with_file(source, "test.orna");
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    let mut program = lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &parsed.value,
        path: ModulePath::new([Name::new("main")]),
    }]);
    typeck(&mut program);
    assert_eq!(program.validate(), Ok(()), "lowered program must validate");
    program
}

fn run_with(body: &str, limits: ExecLimits) -> Result<Value, ExecError> {
    let program = lower_typed(body);
    let run_def: DefId = program
        .modules
        .iter()
        .flat_map(|module| module.items.iter().copied())
        .find(|def| {
            matches!(program.defs[*def].kind, DefKind::Fn(_))
                && program.defs[*def].name.as_str() == "run"
        })
        .expect("the source defines `run`");
    run_entry(&program, run_def, limits).result
}

fn run(body: &str) -> Result<Value, ExecError> {
    run_with(body, LIMITS)
}

fn text(value: &str) -> Result<Value, ExecError> {
    Ok(Value::Text(value.to_owned()))
}

#[test]
fn an_int_interpolates_as_its_decimal_text() {
    assert_eq!(run("pub fn run(): Str = \"n={7}\";\n"), text("n=7"));
    assert_eq!(run("pub fn run(): Str = \"{0 - 7}\";\n"), text("-7"));
}

#[test]
fn a_bool_interpolates_as_true_or_false() {
    assert_eq!(run("pub fn run(): Str = \"{true}/{false}\";\n"), text("true/false"));
}

#[test]
fn a_decimal_interpolates_in_plain_fixed_point() {
    assert_eq!(run("pub fn run(): Str = \"{1.50}\";\n"), text("1.5"));
    assert_eq!(run("pub fn run(): Str = \"{0.05}\";\n"), text("0.05"));
    assert_eq!(run("pub fn run(): Str = \"{120.0}\";\n"), text("120"));
    assert_eq!(run("pub fn run(): Str = \"{0.0 - 0.5}\";\n"), text("-0.5"));
}

#[test]
fn literal_text_and_parts_concatenate_in_order() {
    assert_eq!(run("pub fn run(): Str = \"a{1}b{true}c\";\n"), text("a1btruec"));
    assert_eq!(run("pub fn run(): Str = \"{1}{2}\";\n"), text("12"));
}

#[test]
fn a_user_type_interpolates_through_its_impl_display() {
    let source = format!("use core.display.{{Display}};\n{CONTACT}pub fn run(): Str = \"hi {{contact()}}!\";\n");
    assert_eq!(run(&source), text("hi ada!"));
}

#[test]
fn an_interpolation_past_the_string_limit_is_an_eval_limit_error() {
    let tight = ExecLimits {
        max_string_bytes: 4,
        ..LIMITS
    };
    assert_eq!(
        run_with("pub fn run(): Str = \"{12345}\";\n", tight),
        Err(ExecError::Code("ORNA-EVAL-LIMIT"))
    );
}
