use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_value_v1::Raw;

fn field<'a>(value: &'a Raw, name: &str) -> &'a Raw {
    let Raw::Map(entries) = value else {
        panic!("expected record map, got {value:?}");
    };
    entries
        .iter()
        .find_map(|(key, value)| match key {
            Raw::Text(key) if key == name => Some(value),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing `{name}` in {value:?}"))
}

fn child(node: &Raw, index: usize) -> &Raw {
    let Raw::Array(children) = field(field(node, "slots"), "content") else {
        panic!("presentation content slot must be an array");
    };
    &children[index]
}

fn text(value: &Raw) -> &str {
    let Raw::Text(value) = value else {
        panic!("expected text, got {value:?}");
    };
    value
}

#[test]
fn deeply_nested_helpers_keep_leaf_format_and_typed_optional_value() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference std failed to load: {}", error.code()));
    let value = session
        .submit(include_str!(
            "fixtures/stdlib-presentation-depth-3qbcx.orna"
        ))
        .unwrap_or_else(|error| panic!("nested presentation fixture failed: {}", error.code()))
        .expect("nested presentation returns a node");
    let root = value.raw();

    assert_eq!(
        text(field(field(root, "contract"), "name")),
        "std.ui.details"
    );
    let rows = child(root, 0);
    let stack = child(rows, 0);
    let details = child(stack, 0);
    let inner_rows = child(details, 0);
    let inner_stack = child(inner_rows, 0);

    let code = child(inner_stack, 0);
    assert_eq!(text(field(field(code, "contract"), "name")), "std.ui.code");
    let language = field(field(code, "properties"), "language");
    assert_eq!(text(field(language, "type")), "std.option<std.text>");
    assert_eq!(field(language, "value"), &Raw::Null);

    let formatted = child(inner_stack, 1);
    assert_eq!(
        text(field(field(formatted, "contract"), "name")),
        "std.ui.text"
    );
    let formatted_value = field(field(formatted, "properties"), "value");
    assert_eq!(
        field(formatted_value, "value"),
        &Raw::Text(" deep e\u{301}\n🙂\t ".into())
    );

    let field_node = child(inner_stack, 2);
    let zero = field(field(field(field_node, "properties"), "value"), "value");
    assert_eq!(zero, &Raw::Int(0.into()));
}
