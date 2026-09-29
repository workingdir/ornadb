use orna_syntax_v1::parse_module_with_file;

const ACCEPTED_PROBES: &[(&str, &str)] = &[
    (
        "numeric-calendar",
        include_str!("fixtures/probe-grammar/numeric-calendar.orna"),
    ),
    (
        "contextual-names",
        include_str!("fixtures/probe-grammar/contextual-names.orna"),
    ),
    (
        "unicode-identifier",
        include_str!("fixtures/probe-grammar/unicode-identifier.orna"),
    ),
    (
        "interpolated-string",
        include_str!("fixtures/probe-grammar/interpolated-string.orna"),
    ),
    (
        "string-escapes",
        include_str!("fixtures/probe-grammar/string-escapes.orna"),
    ),
];

#[test]
fn bounded_probe_lexical_forms_are_accepted_by_the_orna_parser() {
    for (name, source) in ACCEPTED_PROBES {
        let parsed = parse_module_with_file(source, format!("{name}.orna"));
        assert!(
            parsed.is_ok(),
            "{name} should parse: {:?}",
            parsed.diagnostics
        );
    }
}

#[test]
fn malformed_calendar_tokens_are_rejected_after_lexical_recognition() {
    let invalid_date = parse_module_with_file(
        include_str!("fixtures/probe-grammar/invalid-date.orna"),
        "invalid-date.orna",
    );
    assert!(
        invalid_date
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "ORNA-LEX-007"),
        "invalid calendar date should retain its lexical diagnostic: {:?}",
        invalid_date.diagnostics
    );

    let invalid_instant = parse_module_with_file(
        include_str!("fixtures/probe-grammar/invalid-instant.orna"),
        "invalid-instant.orna",
    );
    assert!(
        invalid_instant
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "ORNA-LEX-008"),
        "invalid instant should retain its lexical diagnostic: {:?}",
        invalid_instant.diagnostics
    );

    let invalid_scalar = parse_module_with_file(
        include_str!("fixtures/probe-grammar/invalid-scalar-escape.orna"),
        "invalid-scalar-escape.orna",
    );
    assert!(
        invalid_scalar
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "ORNA-LEX-011"),
        "surrogate escape should retain its lexical diagnostic: {:?}",
        invalid_scalar.diagnostics
    );
}
