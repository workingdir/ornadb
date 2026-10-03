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

fn text(value: &Raw) -> &str {
    let Raw::Text(value) = value else {
        panic!("expected text, got {value:?}");
    };
    value
}

#[test]
fn presentation_keeps_empty_unicode_optional_and_column_edge_values() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference std failed to load: {}", error.code()));
    let value = session
        .submit(include_str!(
            "fixtures/stdlib-presentation-format-edge-vh16u.orna"
        ))
        .unwrap_or_else(|error| panic!("presentation edge fixture failed: {}", error.code()))
        .expect("edge fixture returns values");
    let Raw::Array(nodes) = value.raw() else {
        panic!("expected presentation node list, got {:?}", value.raw());
    };
    assert_eq!(nodes.len(), 9);

    assert_eq!(
        text(field(
            field(field(&nodes[0], "properties"), "value"),
            "value"
        )),
        ""
    );
    assert_eq!(
        text(field(
            field(field(&nodes[1], "properties"), "value"),
            "value"
        )),
        " e\u{301} \n🙂\t "
    );
    assert_eq!(
        field(field(field(&nodes[2], "properties"), "value"), "value"),
        &Raw::Bool(false)
    );
    assert_eq!(
        field(field(field(&nodes[3], "properties"), "value"), "value"),
        &Raw::Int(0.into())
    );

    for (node, property) in [(&nodes[4], "language"), (&nodes[5], "title")] {
        let property = field(field(node, "properties"), property);
        assert_eq!(text(field(property, "type")), "std.option<std.text>");
        assert_eq!(field(property, "value"), &Raw::Null);
    }

    let input_value = field(field(&nodes[6], "properties"), "value");
    assert_eq!(text(field(input_value, "type")), "std.option<std.integer>");
    assert_eq!(field(input_value, "value"), &Raw::Null);
    assert_eq!(
        text(field(
            field(field(&nodes[6], "actions"), "change"),
            "input_type"
        )),
        "std.integer"
    );

    assert_eq!(
        field(field(&nodes[7], "properties"), "columns"),
        &Raw::Map(vec![
            (Raw::Text("type".into()), Raw::Text("std.list".into()),),
            (
                Raw::Text("value".into()),
                Raw::Array(vec![
                    Raw::Text("name".into()),
                    Raw::Text("".into()),
                    Raw::Text("name".into()),
                ]),
            ),
        ])
    );

    let Raw::Array(rows) = field(field(&nodes[8], "slots"), "content") else {
        panic!("Details must preserve its nested Rows child");
    };
    let Raw::Array(leaves) = field(field(&rows[0], "slots"), "content") else {
        panic!("Rows must preserve its presentation leaves");
    };
    assert_eq!(
        text(field(field(&leaves[0], "contract"), "name")),
        "std.ui.text"
    );
    assert_eq!(
        text(field(field(&leaves[1], "contract"), "name")),
        "std.ui.code"
    );
    let language = field(field(&leaves[1], "properties"), "language");
    assert_eq!(text(field(language, "type")), "std.option<std.text>");
    assert_eq!(field(language, "value"), &Raw::Null);
}

#[test]
fn input_rejects_a_value_whose_type_disagrees_with_its_action() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference std failed to load: {}", error.code()));
    let error = session
        .submit(
            r#"std.ui.Input("count", "Count", Some("not an integer"), std.ui.action("count-change", as: Int))"#,
        )
        .expect_err("input value type must agree with its typed action");
    assert_eq!(error.code(), "ORNA-EVAL-ARGUMENT");
}
