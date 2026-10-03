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

fn assert_node(value: &Raw, name: &str) {
    assert_eq!(text(field(value, "kind")), "node");
    let contract = field(value, "contract");
    assert_eq!(text(field(contract, "name")), name);
    assert_eq!(text(field(contract, "id")), format!("{name}@1"));
    assert_eq!(text(field(contract, "version")), "1.0");
}

fn assert_typed_property(node: &Raw, name: &str, expected_type: &str, expected: &Raw) {
    let property = field(field(node, "properties"), name);
    assert_eq!(text(field(property, "type")), expected_type);
    assert_eq!(field(property, "value"), expected);
}

#[test]
fn presentation_helpers_preserve_typed_values_children_and_actions() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference std failed to load: {}", error.code()));
    let value = session
        .submit(include_str!(
            "fixtures/stdlib-presentation-depth-sozch.orna"
        ))
        .unwrap_or_else(|error| panic!("presentation helpers failed: {}", error.code()))
        .expect("presentation fixture returns a value");
    let Raw::Array(nodes) = value.raw() else {
        panic!(
            "presentation fixture should produce a node list: {:?}",
            value.raw()
        );
    };
    assert_eq!(nodes.len(), 12);

    assert_node(&nodes[0], "std.ui.text");
    assert_typed_property(
        &nodes[0],
        "value",
        "std.text",
        &Raw::Text("report ready".into()),
    );

    assert_node(&nodes[1], "std.ui.details");
    let Raw::Array(fields) = field(field(&nodes[1], "slots"), "content") else {
        panic!("Details must preserve its Field children");
    };
    assert_eq!(fields.len(), 2);
    assert_node(&fields[0], "std.ui.field");
    assert_typed_property(&fields[0], "label", "std.text", &Raw::Text("Count".into()));
    assert_typed_property(&fields[0], "value", "std.integer", &Raw::Int(42.into()));
    assert_typed_property(
        &fields[1],
        "value",
        "std.list",
        &Raw::Array(vec![Raw::Text("Ada".into()), Raw::Text("Lin".into())]),
    );

    assert_node(&nodes[2], "std.ui.button");
    let action = field(field(&nodes[2], "actions"), "activate");
    assert_eq!(text(field(action, "action_id")), "save-report");
    assert_eq!(text(field(action, "input_type")), "std.text");
    assert_typed_property(&nodes[2], "label", "std.text", &Raw::Text("Save".into()));

    assert_node(&nodes[3], "std.ui.form");
    let Raw::Array(inputs) = field(field(&nodes[3], "slots"), "content") else {
        panic!("Form must preserve its Input child");
    };
    assert_eq!(inputs.len(), 1);
    assert_node(&inputs[0], "std.ui.input");
    let change = field(field(&inputs[0], "actions"), "change");
    assert_eq!(text(field(change, "action_id")), "search-name");
    assert_eq!(text(field(change, "input_type")), "std.text");
    let input_value = field(field(&inputs[0], "properties"), "value");
    assert_eq!(text(field(input_value, "type")), "std.option<std.text>");
    let submit = field(field(&nodes[3], "actions"), "submit");
    assert_eq!(text(field(submit, "action_id")), "submit-search");

    for (node, name) in nodes[4..].iter().zip([
        "std.ui.table",
        "std.ui.tree",
        "std.ui.code",
        "std.ui.diff",
        "std.ui.chart",
        "std.ui.rows",
        "std.ui.cols",
        "std.ui.stack",
    ]) {
        assert_node(node, name);
    }
    assert_typed_property(
        &nodes[4],
        "columns",
        "std.list",
        &Raw::Array(vec![Raw::Text("id".into()), Raw::Text("name".into())]),
    );
    let rows_value = field(field(&nodes[4], "properties"), "rows");
    assert_eq!(text(field(rows_value, "type")), "std.list");
    let Raw::Array(rows) = field(rows_value, "value") else {
        panic!("Table retains its input rows");
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(field(&rows[0], "id"), &Raw::Int(7.into()));
    assert_eq!(field(&rows[0], "name"), &Raw::Text("Ada".into()));

    let tree = field(field(&nodes[5], "properties"), "root");
    assert_eq!(text(field(tree, "type")), "std.record");
    assert_eq!(text(field(field(tree, "value"), "name")), "root");
    assert_eq!(
        field(field(tree, "value"), "children"),
        &Raw::Array(vec![Raw::Text("leaf".into())])
    );

    assert_typed_property(
        &nodes[6],
        "source",
        "std.text",
        &Raw::Text("let answer = 42".into()),
    );
    let diff = field(field(&nodes[7], "properties"), "change");
    assert_eq!(text(field(diff, "type")), "std.record");
    assert_eq!(
        field(field(diff, "value"), "added"),
        &Raw::Array(vec![Raw::Text("new".into())])
    );
    assert_eq!(
        field(field(diff, "value"), "removed"),
        &Raw::Array(vec![Raw::Text("old".into())])
    );
    let series = field(field(&nodes[8], "properties"), "series");
    assert_eq!(text(field(series, "type")), "std.record");
    assert_eq!(text(field(field(series, "value"), "series")), "latency");
    assert_eq!(
        field(field(series, "value"), "points"),
        &Raw::Array(vec![
            Raw::Int(2.into()),
            Raw::Int(3.into()),
            Raw::Int(5.into())
        ])
    );

    for (node, expected) in
        nodes[9..]
            .iter()
            .zip([["left", "right"], ["top", "bottom"], ["first", "second"]])
    {
        let Raw::Array(children) = field(field(node, "slots"), "content") else {
            panic!("container helpers must preserve their children");
        };
        assert_eq!(children.len(), 2);
        for (child, expected) in children.iter().zip(expected) {
            assert_node(child, "std.ui.text");
            assert_typed_property(child, "value", "std.text", &Raw::Text(expected.into()));
        }
    }
}

#[test]
fn button_rejects_values_that_are_not_server_action_descriptors() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference std failed to load: {}", error.code()));
    let error = session
        .submit(r#"std.ui.Button("Save", true)"#)
        .expect_err("a Boolean cannot stand in for an action descriptor");
    assert_eq!(error.code(), "ORNA-EVAL-ARGUMENT");
}
