use orna_conformance_v1::{
    ConformanceAdapter, SemanticAdapter, SourceUnit, StageOutcome,
};
use orna_semantic_v1::{ModuleInput, analyze};

const FIXTURE: &str = include_str!("fixtures/diagnostic-codes.orna");

fn case(name: &str) -> &str {
    let marker = format!("// CASE: {name}\n");
    let rest = FIXTURE
        .split_once(&marker)
        .unwrap_or_else(|| panic!("fixture case is missing: {name}"))
        .1;
    rest.split("// CASE:").next().unwrap_or(rest).trim()
}

fn unit(name: &str) -> SourceUnit {
    SourceUnit {
        fixture_id: format!("diagnostic-{name}"),
        source_id: format!("{name}.orna"),
        parse_as: "module_unit".into(),
        source: case(name).to_owned(),
    }
}

fn assert_parse_code(adapter: &mut SemanticAdapter, name: &str, expected: &str) {
    let source = unit(name);
    match adapter.parse(&source) {
        StageOutcome::Failed(diagnostic) => {
            assert_eq!(adapter.diagnostic_code(&diagnostic), expected, "{name}");
        }
        outcome => panic!("{name} should be rejected by parsing, got {outcome:?}"),
    }
}

fn assert_semantic_code(adapter: &mut SemanticAdapter, name: &str, expected: &str) {
    let source = unit(name);
    let mut observed = Vec::new();
    for outcome in [adapter.resolve(&source), adapter.typecheck(&source)] {
        if let StageOutcome::Failed(diagnostic) = outcome {
            let code = adapter.diagnostic_code(&diagnostic);
            observed.push(code.clone());
            if code == expected {
                return;
            }
        }
    }
    panic!("{name} did not reach stable diagnostic {expected}; observed {observed:?}");
}

#[test]
fn defined_parser_diagnostic_codes_are_reachable_from_real_fixtures() {
    let mut adapter = SemanticAdapter::default();
    for (name, code) in [
        ("A091-001", "ORNA-A091-001"),
        ("A091-005", "ORNA-A091-005"),
        ("A091-006", "ORNA-A091-006"),
        ("A091-010", "ORNA-A091-010"),
        ("A091-011", "ORNA-A091-011"),
        ("RETURN-ARROW", "ORNA091-E-RETURN-ARROW"),
        ("VAR", "ORNA091-E-VAR"),
        ("MATCH", "ORNA091-E-MATCH"),
        ("POSTFIX-QUESTION", "ORNA091-E-POSTFIX-QUESTION"),
        ("CURRENCY", "ORNA091-E-CURRENCY"),
        ("IMPL-FOR", "ORNA091-E-IMPL-FOR"),
        ("BOUND-COLON", "ORNA091-E-BOUND-COLON"),
        ("STATIC-FN", "ORNA091-E-STATIC-FN"),
        ("OPAQUE", "ORNA091-E-OPAQUE"),
        ("FIELD-CONSTRAINT", "ORNA091-E-FIELD-CONSTRAINT"),
    ] {
        assert_parse_code(&mut adapter, name, code);
    }
}

#[test]
fn defined_semantic_diagnostic_codes_are_reachable_from_real_fixtures() {
    let mut adapter = SemanticAdapter::default();
    for (name, code) in [
        ("A091-002", "ORNA-A091-002"),
        ("A091-003", "ORNA-A091-003"),
        ("A091-004", "ORNA-A091-004"),
        ("A091-007", "ORNA-A091-007"),
        ("A091-012", "ORNA-A091-012"),
        ("CURRENCY-SYMBOL", "ORNA091-E-CURRENCY-SYMBOL"),
        ("TRYFROM", "ORNA091-E-TRYFROM"),
        ("CONVERSION-CHAIN", "ORNA091-E-CONVERSION-CHAIN"),
    ] {
        assert_semantic_code(&mut adapter, name, code);
    }

    let legacy_result = analyze(&[ModuleInput::new("RESULT.orna", case("RESULT"))]);
    assert!(
        legacy_result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == "ORNA091-E-RESULT"),
        "semantic analyzer must produce the §33 Result code: {:?}",
        legacy_result.diagnostics
    );
}

#[test]
fn assertion_execution_codes_are_separated_from_compile_time_diagnostics() {
    // These fixture cases exercise the source forms named by §33. The
    // compiler/evaluator accepts the assertions, but cannot produce false
    // assertion outcomes here; ORNA-A091-009 is stream-owned and ORNA-A091-008
    // has no producer in the compiler/evaluator path.
    for name in ["A091-008", "A091-009"] {
        let analysis = analyze(&[ModuleInput::new(
            format!("{name}.orna"),
            case(name),
        )]);
        assert!(analysis.is_ok(), "{name}: {:?}", analysis.diagnostics);
    }
    assert_eq!(
        orna_stream_v1::AssertionDiagnosticCode::TableOrCrossTableFalse.as_str(),
        "ORNA-A091-009"
    );
}
