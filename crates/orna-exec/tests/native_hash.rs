//! The `core.hash` engine natives over lowered source, against the old evaluator's outcomes: SHA-256 digests, the
//! 64-digit lowercase hexadecimal text, and `from_hex` accepting exactly lowercase digits.

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

const HELLO_DIGEST: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
const HEAD: &str = "use core.hash;\n";

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

fn run(source: &str) -> Result<Value, ExecError> {
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

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

/// The bytes of a 64-digit lowercase hexadecimal string.
fn decode(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).expect("hex pair"))
        .collect()
}

#[test]
fn sha256_text_and_to_hex_produce_the_known_digest() {
    assert_eq!(
        run(&format!(
            "{HEAD}pub fn run(): Str = hash.to_hex(hash.sha256_text(\"hello\"));\n"
        )),
        Ok(text(HELLO_DIGEST))
    );
}

#[test]
fn sha256_of_a_blob_hashes_its_bytes() {
    assert_eq!(
        run(&format!(
            "{HEAD}use core.encoding.base64;\n\
             pub fn run(): Str = hash.to_hex(hash.sha256(base64.decode(\"aGVsbG8=\")));\n"
        )),
        Ok(text(HELLO_DIGEST))
    );
}

#[test]
fn from_hex_accepts_exactly_lowercase_digests() {
    assert_eq!(
        run(&format!(
            "{HEAD}pub fn run(): Digest? = hash.from_hex(\"{HELLO_DIGEST}\");\n"
        )),
        Ok(Value::Option(Some(Rc::new(Value::Digest(
            decode(HELLO_DIGEST).try_into().expect("32 bytes")
        )))))
    );
    let uppercase = HELLO_DIGEST.to_uppercase();
    assert_eq!(
        run(&format!(
            "{HEAD}pub fn run(): Digest? = hash.from_hex(\"{uppercase}\");\n"
        )),
        Ok(Value::Option(None))
    );
    assert_eq!(
        run(&format!(
            "{HEAD}pub fn run(): Digest? = hash.from_hex(\"abc\");\n"
        )),
        Ok(Value::Option(None))
    );
}
