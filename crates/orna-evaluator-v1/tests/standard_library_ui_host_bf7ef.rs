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
fn source_button_keeps_an_action_handle_as_introspectable_data() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference std failed to load: {}", error.code()));
    let value = session
        .submit(include_str!("fixtures/stdlib-ui-action-host-bf7ef.orna"))
        .unwrap_or_else(|error| panic!("UI action fixture failed: {}", error.code()))
        .expect("Button returns its presentation node");
    let node = value.raw();
    assert_eq!(
        text(field(field(node, "contract"), "name")),
        "std.ui.button"
    );
    let action = field(field(node, "actions"), "activate");
    assert_eq!(text(field(action, "action_id")), "restart");
    assert_eq!(text(field(action, "input_type")), "std.boolean");
    assert_eq!(field(action, "debug_kind"), &Raw::Null);
}

#[test]
fn source_action_rejects_empty_ids_and_unmapped_input_types() {
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("reference std failed to load: {}", error.code()));
    let empty_id = session
        .submit(r#"std.ui.action("", as: Int)"#)
        .expect_err("an action handle needs a nonempty host identifier");
    assert_eq!(empty_id.code(), "ORNA-EVAL-VALUE");

    let unsupported_type = session
        .submit(r#"std.ui.action("save", as: Date)"#)
        .expect_err("an action handle needs a supported input type");
    assert_eq!(unsupported_type.code(), "ORNA-EVAL-ARGUMENT");
}
