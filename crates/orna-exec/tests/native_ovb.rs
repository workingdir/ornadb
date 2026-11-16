//! The `core.encoding.ovb` engine natives over lowered source: `encode` yields the canonical OVB-1 bytes of its
//! argument, `decode` reads them back at the declared type, and the refusals match the old evaluator's codec:
//! `ORNA-EVAL-VALUE` for bytes that are not one canonical value of the declared type and `ORNA-EVAL-LIMIT` past the
//! collection and string limits.

use std::rc::Rc;

use orna_exec::{ExecError, ExecLimits, Value, run_entry};
use orna_hir::{DefId, DefKind, ModulePath, Name, Program, SourceId, SourceUnit, lower};
use orna_syntax::parse_module_with_file;
use orna_value::{
    CanonicalValue, DURATION_TAG, ENUM_TAG, RECORD_TAG, REFINED_NOMINAL_TAG, Raw, UUID_TAG,
    base64_encode, enum_type_id, enum_variant_id,
};

const LIMITS: ExecLimits = ExecLimits {
    max_steps: 100_000,
    max_depth: 64,
    max_collection_items: 2_048,
    max_string_bytes: 1 << 20,
    max_integer_digits: 1_024,
};

const HEAD: &str = "use core.encoding.ovb;\nuse core.encoding.base64;\n";

/// A nominal record with one defaulted field, decoded at `Pair`.
const PAIR: &str = "pub type Pair {\n    pub a: Int,\n    pub b: Int = 5,\n}\n";

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

/// The Orna expression that decodes the canonical bytes of the map `entries` as a `Pair`.
fn decode_pair(entries: Vec<(&str, Raw)>) -> String {
    let map = Raw::Map(
        entries
            .into_iter()
            .map(|(key, value)| (Raw::Text(key.to_owned()), value))
            .collect(),
    );
    let encoded = base64_encode(&canonical(map));
    format!("ovb.decode<Pair>(base64.decode(\"{encoded}\"))")
}

/// A program defining the `Pair` type and `run` as `expression`.
fn pair_program(expression: &str) -> String {
    format!("{HEAD}{PAIR}pub fn run(): Pair = {expression};\n")
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
fn decode_refuses_a_value_whose_shape_is_not_the_declared_type() {
    assert_eq!(
        run("Int", "ovb.decode<Int>(ovb.encode<Str>(\"a\"))"),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}

#[test]
fn decode_fills_an_omitted_defaulted_field_at_its_declared_default() {
    let expression = decode_pair(vec![("a", Raw::Int(1.into()))]);
    let expected = Value::Record(Rc::from([
        (Name::new("a"), Value::Int(1.into())),
        (Name::new("b"), Value::Int(5.into())),
    ]));
    assert_eq!(run_with(&pair_program(&expression), LIMITS), Ok(expected));
}

#[test]
fn decode_refuses_a_record_missing_a_field_without_a_default() {
    let expression = decode_pair(vec![("b", Raw::Int(2.into()))]);
    assert_eq!(
        run_with(&pair_program(&expression), LIMITS),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}

#[test]
fn decode_refuses_a_record_with_an_undeclared_field() {
    let expression = decode_pair(vec![("a", Raw::Int(1.into())), ("z", Raw::Int(2.into()))]);
    assert_eq!(
        run_with(&pair_program(&expression), LIMITS),
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

/// The Orna expression that decodes the canonical bytes of `raw` at the type `ty`.
fn decode_raw(ty: &str, raw: Raw) -> String {
    let encoded = base64_encode(&canonical(raw));
    format!("ovb.decode<{ty}>(base64.decode(\"{encoded}\"))")
}

/// A refined type whose base is `Int` and whose two predicates bound it to `0..=100`.
const SCORE: &str = "pub type Score = Int {\n    assert >= 0;\n    assert <= 100;\n}\n";

const GBP: &str = "pub type GBP {\n    impl Currency {\n        static code = \"GBP\";\n        static minor_digits = 2;\n    }\n}\npub fn gbp(amount: Decimal): Money<GBP> = Money<GBP>(amount);\n";

const USD: &str = "pub type USD {\n    impl Currency {\n        static code = \"USD\";\n        static minor_digits = 2;\n    }\n}\npub fn usd(amount: Decimal): Money<USD> = Money<USD>(amount);\n";

#[test]
fn decode_reads_a_float_from_its_canonical_bits() {
    let bits = 1.5_f64.to_bits();
    assert_eq!(
        run("Float", &decode_raw("Float", Raw::Float(bits))),
        Ok(Value::Float(bits))
    );
    assert_eq!(
        run("Float", &decode_raw("Float", Raw::Int(1.into()))),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}

#[test]
fn decode_reads_a_duration_from_its_seconds_and_nanoseconds() {
    let body = Raw::Array(vec![Raw::Int(5.into()), Raw::Int(250.into())]);
    assert_eq!(
        run(
            "Duration",
            &decode_raw("Duration", Raw::Tag(DURATION_TAG, Box::new(body)))
        ),
        Ok(Value::Duration {
            seconds: 5.into(),
            nanosecond: 250,
        })
    );
}

fn score_program(expression: &str) -> String {
    format!("{HEAD}{SCORE}pub fn run(): Score = {expression};\n")
}

#[test]
fn decode_checks_each_refinement_of_the_base_value() {
    assert_eq!(
        run_with(
            &score_program(&decode_raw("Score", Raw::Int(50.into()))),
            LIMITS
        ),
        Ok(Value::Int(50.into()))
    );
    for refused in [-1, 101] {
        assert_eq!(
            run_with(
                &score_program(&decode_raw("Score", Raw::Int(refused.into()))),
                LIMITS
            ),
            Err(ExecError::Code("ORNA-A091-008"))
        );
    }
}

#[test]
fn decode_reads_the_refined_wrapper_that_names_its_type() {
    let wrapped = |id: [u8; 16]| {
        Raw::Tag(
            REFINED_NOMINAL_TAG,
            Box::new(Raw::Array(vec![
                Raw::Tag(UUID_TAG, Box::new(Raw::Bytes(id.to_vec()))),
                Raw::Int(50.into()),
            ])),
        )
    };
    assert_eq!(
        run_with(
            &score_program(&decode_raw("Score", wrapped(enum_type_id("main.Score")))),
            LIMITS
        ),
        Ok(Value::Int(50.into()))
    );
    assert_eq!(
        run_with(
            &score_program(&decode_raw("Score", wrapped(enum_type_id("main.Other")))),
            LIMITS
        ),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}

#[test]
fn decode_reads_money_at_its_currency_and_refuses_another() {
    let round_trip = format!(
        "{HEAD}{GBP}pub fn run(): Money<GBP> = ovb.decode<Money<GBP>>(ovb.encode<Money<GBP>>(gbp(1.00)));\n"
    );
    let direct = format!("{HEAD}{GBP}pub fn run(): Money<GBP> = gbp(1.00);\n");
    assert_eq!(
        run_with(&round_trip, LIMITS),
        run_with(&direct, LIMITS),
        "a Money value reads back as the same Money value"
    );
    let other_currency = format!(
        "{HEAD}{GBP}{USD}pub fn run(): Money<GBP> = ovb.decode<Money<GBP>>(ovb.encode<Money<USD>>(usd(1.00)));\n"
    );
    assert_eq!(
        run_with(&other_currency, LIMITS),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}

/// A closed enum with a bare variant and one payload variant, encoded and decoded as `Shade`.
const SHADE: &str = "pub enum Shade {\n    Dark,\n    Tinted { level: Int },\n}\n";

fn shade_program(expression: &str) -> String {
    format!("{HEAD}{SHADE}pub fn run(): Shade = {expression};\n")
}

fn uuid_raw(id: [u8; 16]) -> Raw {
    Raw::Tag(UUID_TAG, Box::new(Raw::Bytes(id.to_vec())))
}

#[test]
fn decode_then_encode_rewrites_an_enum_as_its_canonical_bytes() {
    // Source cannot yet construct an enum value (`ExprKind::Construct` is NotYet in the evaluator), so the enum is
    // decoded from canonical bytes computed independently of the engine, then encoded back.
    let tinted = Raw::Tag(
        ENUM_TAG,
        Box::new(Raw::Array(vec![
            uuid_raw(enum_type_id("main.Shade")),
            uuid_raw(enum_variant_id("main.Shade", "Tinted")),
            Raw::Tag(
                RECORD_TAG,
                Box::new(Raw::Array(vec![
                    Raw::Null,
                    Raw::Array(vec![Raw::Array(vec![
                        Raw::Text("level".to_owned()),
                        Raw::Int(3.into()),
                    ])]),
                ])),
            ),
        ])),
    );
    let bytes = canonical(tinted);
    let encoded = base64_encode(&bytes);
    assert_eq!(
        run_with(
            &format!(
                "{HEAD}{SHADE}pub fn run(): Blob = ovb.encode<Shade>(ovb.decode<Shade>(base64.decode(\"{encoded}\")));\n"
            ),
            LIMITS
        ),
        Ok(blob(&bytes)),
        "an enum decoded from its canonical bytes encodes back to the same bytes"
    );
}

#[test]
fn decode_refuses_an_enum_variant_the_type_does_not_declare() {
    let missing = Raw::Tag(
        ENUM_TAG,
        Box::new(Raw::Array(vec![
            uuid_raw(enum_type_id("main.Shade")),
            uuid_raw(enum_variant_id("main.Shade", "Missing")),
            Raw::Null,
        ])),
    );
    assert_eq!(
        run_with(&shade_program(&decode_raw("Shade", missing)), LIMITS),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}

#[test]
fn encode_writes_a_refined_value_as_its_type_and_base() {
    let wrapped = Raw::Tag(
        REFINED_NOMINAL_TAG,
        Box::new(Raw::Array(vec![
            uuid_raw(enum_type_id("main.Score")),
            Raw::Int(50.into()),
        ])),
    );
    assert_eq!(
        run_with(
            &format!("{HEAD}{SCORE}pub fn run(): Blob = ovb.encode<Score>(50);\n"),
            LIMITS
        ),
        Ok(blob(&canonical(wrapped)))
    );
}
