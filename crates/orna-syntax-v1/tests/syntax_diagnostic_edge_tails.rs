use orna_syntax_v1::parse_module;

#[test]
fn empty_generic_parameter_list_is_rejected_at_closing_angle() {
    let source = include_str!("fixtures/empty-generic-parameters.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(
        diagnostic.message,
        "generic parameter lists require at least one parameter"
    );
    let closing_angle = source.find('>').expect("fixture has a closing angle");
    assert_eq!(diagnostic.span.start, closing_angle);
    assert_eq!(diagnostic.span.end, closing_angle + 1);
}

#[test]
fn empty_generic_type_arguments_are_rejected_at_closing_angle() {
    let source = include_str!("fixtures/empty-generic-type-arguments.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA-PARSE-001");
    assert_eq!(
        diagnostic.message,
        "generic types require at least one type argument"
    );
    let closing_angle = source.find('>').expect("fixture has a closing angle");
    assert_eq!(diagnostic.span.start, closing_angle);
    assert_eq!(diagnostic.span.end, closing_angle + 1);
}

#[test]
fn postfix_question_on_name_gets_the_removed_operator_diagnostic() {
    let source = include_str!("fixtures/postfix-question-on-name.orna");
    let parsed = parse_module(source);

    assert_eq!(parsed.diagnostics.len(), 1, "{:?}", parsed.diagnostics);
    let diagnostic = &parsed.diagnostics[0];
    assert_eq!(diagnostic.code, "ORNA091-E-POSTFIX-QUESTION");
    assert_eq!(
        diagnostic.message,
        "remove postfix `?`; failure propagation is automatic"
    );
    let question = source.find('?').expect("fixture has a question mark");
    assert_eq!(diagnostic.span.start, question);
    assert_eq!(diagnostic.span.end, question + 1);
}

#[test]
fn optional_type_suffix_remains_valid() {
    let parsed = parse_module(include_str!("fixtures/optional-type-question.orna"));
    assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
}
