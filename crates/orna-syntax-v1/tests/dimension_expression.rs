use orna_syntax_v1::{Declaration, TypeExpr, parse_module};

#[test]
fn dimension_products_preserve_terms_and_operators() {
    let parsed = parse_module(
        "pub dim Length; pub dim Time; pub dim Temperature; pub dim Mix = Length^2 * Time / Temperature;",
    );
    assert!(parsed.is_ok(), "diagnostics={:?}", parsed.diagnostics);

    let expression = parsed
        .value
        .items
        .iter()
        .find_map(|item| match &item.declaration {
            Declaration::Dimension {
                name,
                expression: Some(expression),
            } if name == "Mix" => Some(expression),
            _ => None,
        })
        .expect("Mix dimension expression is present");
    assert_eq!(expression.operators, ["*", "/"]);
    assert_eq!(
        expression
            .terms
            .iter()
            .map(|(_, ty, exponent, _)| {
                let TypeExpr::Name { path, arguments, .. } = ty else {
                    panic!("dimension term must be a qualified name: {ty:?}");
                };
                assert!(arguments.is_empty());
                (path.last().unwrap().as_str(), exponent.as_deref())
            })
            .collect::<Vec<_>>(),
        [("Length", Some("2")), ("Time", None), ("Temperature", None)]
    );
}
