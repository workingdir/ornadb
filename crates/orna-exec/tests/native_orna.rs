//! The `core.encoding.orna` engine natives over lowered source: `encode` followed by `decode` returns the value it began
//! with, and `decode` refuses text that is not one canonical expression of the target type with `ORNA-EVAL-VALUE`.

use std::rc::Rc;

use num_bigint::BigInt;
use orna_exec::{ExecError, ExecLimits, Value, run_entry};
use orna_hir::{DefId, DefKind, ModulePath, Name, Program, SourceId, SourceUnit, lower};
use orna_syntax::parse_module_with_file;

const LIMITS: ExecLimits = ExecLimits {
    max_steps: 100_000,
    max_depth: 64,
    max_collection_items: 2_048,
    max_string_bytes: 1 << 20,
    max_integer_digits: 1_024,
};

const HEAD: &str = "use core.encoding.orna;\n";

const PAIR: &str = "type Pair {\n    pub a: Int,\n    pub b: Str,\n}\n";

fn lower_one(source: &str) -> Program {
    let parsed = parse_module_with_file(source, "test.orna");
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    let program = lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &parsed.value,
        path: ModulePath::new([Name::new("main")]),
    }]);
    assert_eq!(program.validate(), Ok(()), "lowered program must validate");
    program
}

/// The program `<types> pub fn run(): <result_type> = <expression>;` behind the codec import. The expression is spliced in
/// verbatim so that the `\u{...}` escapes of Orna string literals are not read as format placeholders.
fn source(types: &str, result_type: &str, expression: &str) -> String {
    let mut output = String::from(HEAD);
    output.push_str(types);
    output.push_str("pub fn run(): ");
    output.push_str(result_type);
    output.push_str(" = ");
    output.push_str(expression);
    output.push_str(";\n");
    output
}

fn run_with(source: &str) -> Result<Value, ExecError> {
    let program = lower_one(source);
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

fn run(result_type: &str, expression: &str) -> Result<Value, ExecError> {
    run_with(&source("", result_type, expression))
}

/// An Orna string literal whose value is `text`: quotes, backslashes and braces are escaped.
fn literal(text: &str) -> String {
    let mut out = String::from("\"");
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '{' => out.push_str("\\u{7b}"),
            '}' => out.push_str("\\u{7d}"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// `orna.decode<target>("<text>")`. The target type is written at the call, as the lowerer reads type arguments only
/// from the call site.
fn decode_of(target: &str, text: &str) -> String {
    format!("orna.decode<{target}>({})", literal(text))
}

fn int(value: i64) -> Value {
    Value::Int(BigInt::from(value))
}

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

fn field(name: &str, value: Value) -> (Name, Value) {
    (Name::new(name), value)
}

fn record(fields: Vec<(Name, Value)>) -> Value {
    Value::Record(Rc::from(fields))
}

#[test]
fn scalars_round_trip_through_encode_and_decode() {
    assert_eq!(
        run("Int", "orna.decode<Int>(orna.encode<Int>(-42))"),
        Ok(int(-42))
    );
    assert_eq!(
        run("Bool", "orna.decode<Bool>(orna.encode<Bool>(true))"),
        Ok(Value::Bool(true))
    );
    assert_eq!(
        run(
            "Str",
            "orna.decode<Str>(orna.encode<Str>(\"a\\\"b\\u{7b}c\\u{7d}\\n\"))"
        ),
        Ok(text("a\"b{c}\n"))
    );
}

#[test]
fn a_record_round_trips_through_encode_and_decode() {
    let types = format!("{PAIR}pub fn pair(): Pair = Pair {{ a: 1, b: \"x\" }};\n");
    let expression = "orna.decode<Pair>(orna.encode<Pair>(pair()))";
    let expected = record(vec![field("a", int(1)), field("b", text("x"))]);
    assert_eq!(run_with(&source(&types, "Pair", expression)), Ok(expected));
}

#[test]
fn a_record_with_an_unknown_or_repeated_key_is_refused() {
    let unknown = decode_of("Pair", "Pair { a: 1, b: \"x\", c: 2 }");
    assert_eq!(
        run_with(&source(PAIR, "Pair", &unknown)),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
    let repeated = decode_of("Pair", "Pair { a: 1, a: 2, b: \"x\" }");
    assert_eq!(
        run_with(&source(PAIR, "Pair", &repeated)),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}

#[test]
fn a_required_field_that_is_omitted_is_refused() {
    let expression = decode_of("Pair", "Pair { a: 1 }");
    assert_eq!(
        run_with(&source(PAIR, "Pair", &expression)),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}

#[test]
fn text_of_the_wrong_type_is_refused() {
    assert_eq!(
        run("Int", &decode_of("Int", "\"1\"")),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
    assert_eq!(
        run("Int", &decode_of("Int", "1.5")),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
    assert_eq!(
        run("Str", &decode_of("Str", "1")),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
    assert_eq!(
        run("Bool", &decode_of("Bool", "1")),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}

#[test]
fn text_that_is_not_one_canonical_expression_is_refused() {
    assert_eq!(
        run("Int", &decode_of("Int", "1 + 2")),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
    assert_eq!(
        run("Int", &decode_of("Int", "1 +")),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
    assert_eq!(
        run("Int", &decode_of("Int", "1 2")),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
    assert_eq!(
        run("Int", &decode_of("Int", "")),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}
