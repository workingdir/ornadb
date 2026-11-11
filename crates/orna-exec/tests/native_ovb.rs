//! The `core.encoding.ovb` engine natives over lowered source: `encode` yields the canonical OVB-1 bytes of its
//! argument, `decode` reads them back, and the refusals match the old evaluator's codec: `ORNA-EVAL-VALUE` for bytes
//! that are not one canonical value and `ORNA-EVAL-LIMIT` past the collection and string limits.

use std::rc::Rc;

use orna_exec::{ExecError, ExecLimits, Value, run_entry};
use orna_hir::{DefId, DefKind, ModulePath, Name, Program, SourceId, SourceUnit, lower};
use orna_syntax::parse_module_with_file;
use orna_value::{CanonicalValue, Raw};

const LIMITS: ExecLimits = ExecLimits {
    max_steps: 100_000,
    max_depth: 64,
    max_collection_items: 2_048,
    max_string_bytes: 1 << 20,
    max_integer_digits: 1_024,
};

const HEAD: &str = "use core.encoding.ovb;\nuse core.encoding.base64;\n";

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
    let user = ModulePath::new([Name::new("main")]);
    let diagnostics: Vec<String> = program
        .modules
        .iter()
        .filter(|module| module.path == user)
        .flat_map(|module| {
            module
                .diagnostics
                .iter()
                .map(|diagnostic| format!("{diagnostic:?}"))
        })
        .collect();
    assert!(
        diagnostics.is_empty(),
        "lowering diagnostics: {diagnostics:?}"
    );
    assert_eq!(program.validate(), Ok(()), "lowered program must validate");
    program
}

/// The program `pub fn run(): <result_type> = <expression>;` behind the codec imports. The expression is spliced in
/// verbatim so that the `\u{...}` escapes of the Orna string literals are not read as format placeholders.
fn source(result_type: &str, expression: &str) -> String {
    let mut output = String::from(HEAD);
    output.push_str("pub fn run(): ");
    output.push_str(result_type);
    output.push_str(" = ");
    output.push_str(expression);
    output.push_str(";\n");
    output
}

fn run_with(source: &str, limits: ExecLimits) -> Result<Value, ExecError> {
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
    run_entry(&program, run_def, limits).result
}

fn run(result_type: &str, expression: &str) -> Result<Value, ExecError> {
    run_with(&source(result_type, expression), LIMITS)
}

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

fn blob(bytes: &[u8]) -> Value {
    Value::Blob(Rc::from(bytes))
}

/// The canonical OVB-1 bytes of `raw`, computed independently of the engine.
fn canonical(raw: Raw) -> Vec<u8> {
    CanonicalValue::new(raw)
        .expect("test value is canonical")
        .encode()
        .expect("test value encodes")
}

#[test]
fn encode_yields_the_canonical_ovb_bytes_of_its_argument() {
    assert_eq!(
        run("Blob", "ovb.encode<Str>(\"a\")"),
        Ok(blob(&canonical(Raw::Text("a".to_owned()))))
    );
    assert_eq!(
        run("Blob", "ovb.encode<Int>(-42)"),
        Ok(blob(&canonical(Raw::Int((-42).into()))))
    );
}

#[test]
fn decode_reads_back_what_encode_wrote() {
    assert_eq!(
        run("Str", "ovb.decode<Str>(ovb.encode<Str>(\"hello\"))"),
        Ok(text("hello"))
    );
    assert_eq!(
        run("Int", "ovb.decode<Int>(ovb.encode<Int>(-42))"),
        Ok(Value::Int((-42).into()))
    );
    assert_eq!(
        run("Bool", "ovb.decode<Bool>(ovb.encode<Bool>(true))"),
        Ok(Value::Bool(true))
    );
}

#[test]
fn decode_refuses_bytes_that_are_not_one_canonical_value() {
    assert_eq!(
        run("Int", "ovb.decode<Int>(base64.decode(\"/w==\"))"),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
    assert_eq!(
        run("Int", "ovb.decode<Int>(base64.decode(\"\"))"),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}

#[test]
fn encode_refuses_output_past_the_collection_limit() {
    let limits = ExecLimits {
        max_collection_items: 4,
        ..LIMITS
    };
    assert_eq!(
        run_with(&source("Blob", "ovb.encode<Str>(\"abcdefghij\")"), limits),
        Err(ExecError::Code("ORNA-EVAL-LIMIT"))
    );
}
