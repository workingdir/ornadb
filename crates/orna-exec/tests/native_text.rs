//! The `core.text` engine natives over lowered source, against the old evaluator's outcomes: the Unicode 16.0.0 contract
//! of the pinned text module, the empty and separator cases of `split` and `join`, the `ORNA-EVAL-VALUE` refusal of an
//! unknown normalisation form, and the `ORNA-EVAL-LIMIT` refusals of a collection or string over its limit.

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

const HEAD: &str = "use core.text;\n";

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

/// The program `pub fn run(): <result_type> = <expression>;` behind the `core.text` import. The expression is spliced in
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

fn list(values: &[&str]) -> Value {
    Value::List(Rc::from(
        values.iter().map(|value| text(value)).collect::<Vec<_>>(),
    ))
}

#[test]
fn trim_removes_unicode_white_space_at_both_ends() {
    assert_eq!(
        run(
            "Str",
            "text.trim(\"\\u{00A0}\\u{2007} A\\u{2003}B \\u{202F}\")"
        ),
        Ok(text("A\u{2003}B"))
    );
}

#[test]
fn split_with_an_empty_separator_yields_scalar_values() {
    assert_eq!(
        run("[Str]", "text.split(\"🙂a🙂\", \"\")"),
        Ok(list(&["🙂", "a", "🙂"]))
    );
    assert_eq!(run("[Str]", "text.split(\"\", \"\")"), Ok(list(&[])));
}

#[test]
fn split_keeps_leading_trailing_and_interior_empty_fields() {
    assert_eq!(
        run("[Str]", "text.split(\",a,,β,\", \",\")"),
        Ok(list(&["", "a", "", "β", ""]))
    );
}

#[test]
fn join_separates_elements_and_empty_join_is_empty() {
    assert_eq!(
        run("Str", "text.join([\"a\", \"β\", \"\"], \":\")"),
        Ok(text("a:β:"))
    );
    assert_eq!(run("Str", "text.join([], \":\")"), Ok(text("")));
}

#[test]
fn starts_ends_and_contains_match_by_scalar_text() {
    assert_eq!(
        run("Bool", "text.starts_with(\"βeta\", \"β\")"),
        Ok(Value::Bool(true))
    );
    assert_eq!(
        run("Bool", "text.ends_with(\"βeta\", \"ta\")"),
        Ok(Value::Bool(true))
    );
    assert_eq!(
        run("Bool", "text.contains(\"aβa\", \"β\")"),
        Ok(Value::Bool(true))
    );
}

#[test]
fn replace_is_non_overlapping_and_left_to_right() {
    assert_eq!(
        run("Str", "text.replace(\"aaaa\", \"aa\", \"x\")"),
        Ok(text("xx"))
    );
}

#[test]
fn lower_applies_the_final_sigma_rule() {
    assert_eq!(run("Str", "text.lower(\"ΟΣ\")"), Ok(text("ος")));
    assert_eq!(
        run("Str", "text.lower(\"Ο\\u{0301}Σ\")"),
        Ok(text("ο\u{0301}ς"))
    );
}

#[test]
fn upper_maps_full_case_expansions() {
    assert_eq!(run("Str", "text.upper(\"ßﬃ\")"), Ok(text("SSFFI")));
}

#[test]
fn lower_and_upper_follow_unicode_16_titlecase_letters() {
    assert_eq!(
        run("Str", "text.lower(\"\\u{1C89}\")"),
        Ok(text("\u{1C8A}"))
    );
    assert_eq!(
        run("Str", "text.upper(\"\\u{1C8A}\")"),
        Ok(text("\u{1C89}"))
    );
}

#[test]
fn normalise_composes_and_decomposes_the_two_canonical_forms() {
    assert_eq!(
        run("Str", "text.normalise(\"A\\u{030A}\", \"NFC\")"),
        Ok(text("Å"))
    );
    assert_eq!(
        run("Str", "text.normalise(\"Å\", \"NFD\")"),
        Ok(text("A\u{030A}"))
    );
}

#[test]
fn an_unknown_normalisation_form_is_refused_as_a_value_error() {
    assert_eq!(
        run("Str", "text.normalise(\"a\", \"NFKC\")"),
        Err(ExecError::Code("ORNA-EVAL-VALUE"))
    );
}

#[test]
fn a_join_longer_than_the_string_limit_is_refused() {
    let limits = ExecLimits {
        max_string_bytes: 4,
        ..LIMITS
    };
    assert_eq!(
        run_with(
            &source("Str", "text.join([\"aaa\", \"bbb\"], \":\")"),
            limits
        ),
        Err(ExecError::Code("ORNA-EVAL-LIMIT"))
    );
}

#[test]
fn a_split_of_more_elements_than_the_collection_limit_is_refused() {
    let limits = ExecLimits {
        max_collection_items: 2,
        ..LIMITS
    };
    assert_eq!(
        run_with(&source("[Str]", "text.split(\"abc\", \"\")"), limits),
        Err(ExecError::Code("ORNA-EVAL-LIMIT"))
    );
}

#[test]
fn a_join_of_more_elements_than_the_collection_limit_is_refused() {
    let limits = ExecLimits {
        max_collection_items: 2,
        ..LIMITS
    };
    assert_eq!(
        run_with(
            &source("Str", "text.join([\"a\", \"b\", \"c\"], \"\")"),
            limits
        ),
        Err(ExecError::Code("ORNA-EVAL-LIMIT"))
    );
}
