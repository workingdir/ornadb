//! Explicit `From` conversion at run time (CONVERT-001, CONVERT-1 step 5): `Target.from(source)` runs the nested
//! `impl From<Source>` that typeck selected for the source's type, by that implementation's identity. A chained
//! conversion is two calls, each running its own implementation: there is no implicit chaining (CONVERT-1 step 7).

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

fn run(body: &str) -> Result<Value, ExecError> {
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
    run_entry(&program, run_def, LIMITS).result
}

/// `Temperature` nests one `From<Int>` implementation, which builds it from the integer.
const TEMPERATURE: &str = "pub type Temperature {\n\
    value: Int,\n\
    impl From<Int> {\n\
        fn from(value): Temperature { Temperature { value: value } }\n\
    }\n\
}\n";

#[test]
fn explicit_from_runs_the_selected_nested_implementation() {
    assert_eq!(
        run(&format!(
            "{TEMPERATURE}pub fn run(): Bool = Temperature.from(21).value == 21;\n"
        )),
        Ok(Value::Bool(true))
    );
}

#[test]
fn each_source_runs_its_own_from_implementation() {
    let source = "pub type Temperature {\n\
        value: Int,\n\
        impl From<Int> {\n\
            fn from(value): Temperature { Temperature { value: value } }\n\
        }\n\
        impl From<Str> {\n\
            fn from(text): Temperature { Temperature { value: 0 } }\n\
        }\n\
    }\n\
    pub fn run(): Bool = Temperature.from(21).value == 21 && Temperature.from(\"21\").value == 0;\n";
    assert_eq!(run(source), Ok(Value::Bool(true)));
}

#[test]
fn a_chained_conversion_runs_each_call_in_turn() {
    let source = "pub type Celsius {\n\
        value: Int,\n\
        impl From<Int> {\n\
            fn from(value): Celsius { Celsius { value: value } }\n\
        }\n\
    }\n\
    pub type Kelvin {\n\
        value: Int,\n\
        impl From<Celsius> {\n\
            fn from(celsius): Kelvin { Kelvin { value: celsius.value + 273 } }\n\
        }\n\
    }\n\
    pub fn run(): Bool = Kelvin.from(Celsius.from(20)).value == 293;\n";
    assert_eq!(run(source), Ok(Value::Bool(true)));
}

/// `Wrapper<T>` nests a `From<T>` implementation: the selected source is substituted into the target's arguments.
#[test]
fn a_generic_target_runs_its_implementation_at_the_substituted_source() {
    let source = "pub type Wrapper<T> {\n\
        inner: T,\n\
        impl From<T> {\n\
            fn from(inner): Wrapper<T> { Wrapper { inner: inner } }\n\
        }\n\
    }\n\
    pub fn run(): Bool = Wrapper.from(21).inner == 21;\n";
    assert_eq!(run(source), Ok(Value::Bool(true)));
}

#[test]
fn a_conversion_inside_an_implementation_runs_its_own_call() {
    let source = "pub type Email {\n\
        address: Str,\n\
        impl From<Str> {\n\
            fn from(text) = Email { address: text };\n\
        }\n\
    }\n\
    pub type Mailbox {\n\
        email: Email,\n\
        impl From<Str> {\n\
            fn from(text) = Mailbox { email: Email.from(text) };\n\
        }\n\
    }\n\
    pub fn run(): Bool = Mailbox.from(\"ada@example.test\").email.address == \"ada@example.test\";\n";
    assert_eq!(run(source), Ok(Value::Bool(true)));
}

/// `Reciprocal` nests a `From<Int>` implementation that divides by its source, so a zero source fails the conversion.
const RECIPROCAL: &str = "pub type Reciprocal {\n\
    value: Int,\n\
    impl From<Int> {\n\
        fn from(divisor): Reciprocal { Reciprocal { value: 100 / divisor } }\n\
    }\n\
}\n";

/// A failing `From` fails the conversion with the error it raised, and the activation aborts (CONVERT-004, CONVERT-010).
#[test]
fn a_failing_from_implementation_fails_the_conversion_with_its_error() {
    assert_eq!(
        run(&format!(
            "{RECIPROCAL}pub fn run(): Bool = Reciprocal.from(0).value == 0;\n"
        )),
        Err(ExecError::Code("ORNA-EVAL-DIVIDE-BY-ZERO"))
    );
}

/// A fallible `From` implementation converts a source it accepts: the failure path above is not taken (CONVERT-004).
#[test]
fn a_fallible_from_implementation_converts_a_valid_source() {
    assert_eq!(
        run(&format!(
            "{RECIPROCAL}pub fn run(): Bool = Reciprocal.from(4).value == 25;\n"
        )),
        Ok(Value::Bool(true))
    );
}
