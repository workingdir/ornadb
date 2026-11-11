//! The `core.encoding.base64` engine natives over lowered source, against the old evaluator's outcomes: the padding
//! matrix of the old fixtures, the strict RFC 4648 refusals, and the `ORNA-EVAL-LIMIT` refusals of `decode`'s string
//! and decoded-byte limits.

use std::rc::Rc;

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

const HEAD: &str = "use core.encoding.base64;\n";

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

/// The program `pub fn run(): <result_type> = <expression>;` behind the codec import. The expression is spliced in
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

#[test]
fn decode_yields_the_old_evaluator_bytes_for_each_padding_form() {
    assert_eq!(run("Blob", "base64.decode(\"\")"), Ok(blob(&[])));
    assert_eq!(run("Blob", "base64.decode(\"AA==\")"), Ok(blob(&[0x00])));
    assert_eq!(
        run("Blob", "base64.decode(\"AAE=\")"),
        Ok(blob(&[0x00, 0x01]))
    );
    assert_eq!(
        run("Blob", "base64.decode(\"AAEC\")"),
        Ok(blob(&[0x00, 0x01, 0x02]))
    );
    assert_eq!(run("Blob", "base64.decode(\"/w==\")"), Ok(blob(&[0xff])));
    assert_eq!(
        run("Blob", "base64.decode(\"/+4=\")"),
        Ok(blob(&[0xff, 0xee]))
    );
    assert_eq!(
        run("Blob", "base64.decode(\"AP8AQQ==\")"),
        Ok(blob(&[0x00, 0xff, 0x00, 0x41]))
    );
}

#[test]
fn encode_is_the_canonical_standard_form_and_round_trips() {
    assert_eq!(
        run("Str", "base64.encode(base64.decode(\"\"))"),
        Ok(text(""))
    );
    assert_eq!(
        run("Str", "base64.encode(base64.decode(\"AA==\"))"),
        Ok(text("AA=="))
    );
    assert_eq!(
        run("Str", "base64.encode(base64.decode(\"AAE=\"))"),
        Ok(text("AAE="))
    );
    assert_eq!(
        run("Str", "base64.encode(base64.decode(\"AAEC\"))"),
        Ok(text("AAEC"))
    );
    assert_eq!(
        run("Str", "base64.encode(base64.decode(\"/+4=\"))"),
        Ok(text("/+4="))
    );
    assert_eq!(
        run("Str", "base64.encode(base64.decode(\"AP8AQQ==\"))"),
        Ok(text("AP8AQQ=="))
    );
}

#[test]
fn decode_refuses_text_that_is_not_strict_standard_base64() {
    // Missing padding, whitespace, and the URL-safe alphabet are not standard Base64 (the old decoder's refusal).
    assert_eq!(
        run("Blob", "base64.decode(\"AA\")"),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
    assert_eq!(
        run("Blob", "base64.decode(\"A A=\")"),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
    assert_eq!(
        run("Blob", "base64.decode(\"-_8=\")"),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}

#[test]
fn decode_over_the_string_limit_is_refused_before_decoding() {
    let limits = ExecLimits {
        max_string_bytes: 4,
        ..LIMITS
    };
    let program = source("Blob", "base64.decode(\"AAECAw==\")");
    assert_eq!(
        run_with(&program, limits),
        Err(ExecError::Code("ORNA-EVAL-LIMIT"))
    );
}

#[test]
fn decoded_bytes_over_the_collection_limit_are_refused() {
    let limits = ExecLimits {
        max_collection_items: 3,
        ..LIMITS
    };
    let program = source("Blob", "base64.decode(\"AAECAw==\")");
    assert_eq!(
        run_with(&program, limits),
        Err(ExecError::Code("ORNA-EVAL-LIMIT"))
    );
}
