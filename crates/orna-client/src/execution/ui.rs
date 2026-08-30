use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StandardUiConstructorKind {
    Text,
    Button,
    Panel,
    Row,
    Column,
    TextInput,
    Tabs,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StandardUiConstructorParameterKind {
    Text,
    Boolean,
    Content,
}

#[derive(Clone, Copy)]
pub(super) struct StandardUiConstructorSpec {
    function: FunctionId,
    revision: FunctionRevisionId,
    identity: &'static str,
    node_contract: &'static str,
    kind: StandardUiConstructorKind,
    parameters: &'static [(ParameterId, StandardUiConstructorParameterKind)],
}

const STD_UI_TEXT_CONSTRUCTOR_PARAMETERS: &[(ParameterId, StandardUiConstructorParameterKind)] =
    &[(
        STD_UI_TEXT_PARAMETER_ID,
        StandardUiConstructorParameterKind::Text,
    )];
const STD_UI_BUTTON_CONSTRUCTOR_PARAMETERS: &[(ParameterId, StandardUiConstructorParameterKind)] =
    &[
        (
            STD_UI_BUTTON_LABEL_PARAMETER_ID,
            StandardUiConstructorParameterKind::Text,
        ),
        (
            STD_UI_BUTTON_ENABLED_PARAMETER_ID,
            StandardUiConstructorParameterKind::Boolean,
        ),
    ];
const STD_UI_PANEL_CONSTRUCTOR_PARAMETERS: &[(ParameterId, StandardUiConstructorParameterKind)] =
    &[(
        STD_UI_PANEL_CONTENT_PARAMETER_ID,
        StandardUiConstructorParameterKind::Content,
    )];
const STD_UI_ROW_CONSTRUCTOR_PARAMETERS: &[(ParameterId, StandardUiConstructorParameterKind)] =
    &[(
        STD_UI_ROW_CONTENT_PARAMETER_ID,
        StandardUiConstructorParameterKind::Content,
    )];
const STD_UI_COLUMN_CONSTRUCTOR_PARAMETERS: &[(ParameterId, StandardUiConstructorParameterKind)] =
    &[(
        STD_UI_COLUMN_CONTENT_PARAMETER_ID,
        StandardUiConstructorParameterKind::Content,
    )];
const STD_UI_TEXT_INPUT_CONSTRUCTOR_PARAMETERS: &[(
    ParameterId,
    StandardUiConstructorParameterKind,
)] = &[
    (
        STD_UI_TEXT_INPUT_TEXT_PARAMETER_ID,
        StandardUiConstructorParameterKind::Text,
    ),
    (
        STD_UI_TEXT_INPUT_PLACEHOLDER_PARAMETER_ID,
        StandardUiConstructorParameterKind::Text,
    ),
    (
        STD_UI_TEXT_INPUT_ENABLED_PARAMETER_ID,
        StandardUiConstructorParameterKind::Boolean,
    ),
];
const STD_UI_TABS_CONSTRUCTOR_PARAMETERS: &[(ParameterId, StandardUiConstructorParameterKind)] =
    &[(
        STD_UI_TABS_CONTENT_PARAMETER_ID,
        StandardUiConstructorParameterKind::Content,
    )];

const STD_UI_TEXT_CONSTRUCTOR: StandardUiConstructorSpec = StandardUiConstructorSpec {
    function: STD_UI_TEXT_FUNCTION_ID,
    revision: STD_UI_TEXT_FUNCTION_REVISION_ID,
    identity: STD_UI_TEXT_RUNTIME_CONTRACT,
    node_contract: "std.ui.text",
    kind: StandardUiConstructorKind::Text,
    parameters: STD_UI_TEXT_CONSTRUCTOR_PARAMETERS,
};
const STD_UI_BUTTON_CONSTRUCTOR: StandardUiConstructorSpec = StandardUiConstructorSpec {
    function: STD_UI_BUTTON_FUNCTION_ID,
    revision: STD_UI_BUTTON_FUNCTION_REVISION_ID,
    identity: STD_UI_BUTTON_RUNTIME_CONTRACT,
    node_contract: "std.ui.button",
    kind: StandardUiConstructorKind::Button,
    parameters: STD_UI_BUTTON_CONSTRUCTOR_PARAMETERS,
};
const STD_UI_PANEL_CONSTRUCTOR: StandardUiConstructorSpec = StandardUiConstructorSpec {
    function: STD_UI_PANEL_FUNCTION_ID,
    revision: STD_UI_PANEL_FUNCTION_REVISION_ID,
    identity: STD_UI_PANEL_RUNTIME_CONTRACT,
    node_contract: "std.ui.panel",
    kind: StandardUiConstructorKind::Panel,
    parameters: STD_UI_PANEL_CONSTRUCTOR_PARAMETERS,
};
const STD_UI_ROW_CONSTRUCTOR: StandardUiConstructorSpec = StandardUiConstructorSpec {
    function: STD_UI_ROW_FUNCTION_ID,
    revision: STD_UI_ROW_FUNCTION_REVISION_ID,
    identity: STD_UI_ROW_RUNTIME_CONTRACT,
    node_contract: "std.ui.row",
    kind: StandardUiConstructorKind::Row,
    parameters: STD_UI_ROW_CONSTRUCTOR_PARAMETERS,
};
const STD_UI_COLUMN_CONSTRUCTOR: StandardUiConstructorSpec = StandardUiConstructorSpec {
    function: STD_UI_COLUMN_FUNCTION_ID,
    revision: STD_UI_COLUMN_FUNCTION_REVISION_ID,
    identity: STD_UI_COLUMN_RUNTIME_CONTRACT,
    node_contract: "std.ui.column",
    kind: StandardUiConstructorKind::Column,
    parameters: STD_UI_COLUMN_CONSTRUCTOR_PARAMETERS,
};
const STD_UI_TEXT_INPUT_CONSTRUCTOR: StandardUiConstructorSpec = StandardUiConstructorSpec {
    function: STD_UI_TEXT_INPUT_FUNCTION_ID,
    revision: STD_UI_TEXT_INPUT_FUNCTION_REVISION_ID,
    identity: STD_UI_TEXT_INPUT_RUNTIME_CONTRACT,
    node_contract: "std.ui.text_input",
    kind: StandardUiConstructorKind::TextInput,
    parameters: STD_UI_TEXT_INPUT_CONSTRUCTOR_PARAMETERS,
};
const STD_UI_TABS_CONSTRUCTOR: StandardUiConstructorSpec = StandardUiConstructorSpec {
    function: STD_UI_TABS_FUNCTION_ID,
    revision: STD_UI_TABS_FUNCTION_REVISION_ID,
    identity: STD_UI_TABS_RUNTIME_CONTRACT,
    node_contract: "std.ui.tabs",
    kind: StandardUiConstructorKind::Tabs,
    parameters: STD_UI_TABS_CONSTRUCTOR_PARAMETERS,
};

pub(super) fn standard_ui_constructor_spec(
    active: &ActiveDatabaseRevision,
    context: ClientExecutionContext,
    identity: &str,
) -> Option<&'static StandardUiConstructorSpec> {
    // Application definitions retain precedence. A user-owned function that
    // happens to spell a standard contract must remain a generic external
    // contract, even if it reuses one of the reserved identities.
    if context.pair() != active.pair()
        || active
            .catalogue()
            .function_by_id(context.function())
            .is_some()
    {
        return None;
    }
    let spec = match context.function() {
        STD_UI_TEXT_FUNCTION_ID => &STD_UI_TEXT_CONSTRUCTOR,
        STD_UI_BUTTON_FUNCTION_ID => &STD_UI_BUTTON_CONSTRUCTOR,
        STD_UI_PANEL_FUNCTION_ID => &STD_UI_PANEL_CONSTRUCTOR,
        STD_UI_ROW_FUNCTION_ID => &STD_UI_ROW_CONSTRUCTOR,
        STD_UI_COLUMN_FUNCTION_ID => &STD_UI_COLUMN_CONSTRUCTOR,
        STD_UI_TEXT_INPUT_FUNCTION_ID => &STD_UI_TEXT_INPUT_CONSTRUCTOR,
        STD_UI_TABS_FUNCTION_ID => &STD_UI_TABS_CONSTRUCTOR,
        _ => return None,
    };
    (spec.function == context.function()
        && spec.revision == context.function_revision
        && spec.identity == identity)
        .then_some(spec)
}

fn invalid_ui_constructor_value(
    context: ClientExecutionContext,
    source: OpaqueValueError,
) -> Box<ClientExecutionError> {
    Box::new(ClientExecutionError::InvalidOpaqueValue {
        context,
        source: ClientOpaqueValueError::Value(source),
    })
}

fn invalid_ui_constructor_registry(
    context: ClientExecutionContext,
    source: RegisteredOpaqueCodecsError,
) -> Box<ClientExecutionError> {
    Box::new(ClientExecutionError::InvalidOpaqueValue {
        context,
        source: ClientOpaqueValueError::Registry(Box::new(source)),
    })
}

fn ui_constructor_parameter_matches(
    value: &RuntimeValue,
    kind: StandardUiConstructorParameterKind,
) -> bool {
    match kind {
        StandardUiConstructorParameterKind::Text => matches!(value, RuntimeValue::Text(_)),
        StandardUiConstructorParameterKind::Boolean => matches!(value, RuntimeValue::Boolean(_)),
        StandardUiConstructorParameterKind::Content => {
            matches!(value, RuntimeValue::Opaque(opaque) if opaque.opaque_type() == STD_UI_TYPE_ID)
        }
    }
}

fn ui_constructor_text_property(value: &str) -> Value {
    let mut property = Map::new();
    property.insert(
        "type".to_owned(),
        Value::String("std.types.text".to_owned()),
    );
    property.insert("value".to_owned(), Value::String(value.to_owned()));
    Value::Object(property)
}

fn ui_constructor_boolean_property(value: bool) -> Value {
    let mut property = Map::new();
    property.insert(
        "type".to_owned(),
        Value::String("std.types.boolean".to_owned()),
    );
    property.insert("value".to_owned(), Value::Bool(value));
    Value::Object(property)
}

pub(super) fn decode_ui_constructor_body(payload: &[u8]) -> Result<Value, OpaqueValueError> {
    let magic = UI_MAGIC.as_bytes();
    let prefix_length = magic
        .len()
        .checked_add(4)
        .ok_or(OpaqueValueError::InvalidFrameLength {
            opaque_type: STD_UI_TYPE_ID,
        })?;
    if payload.len() < prefix_length || !payload.starts_with(magic) {
        return Err(if payload.starts_with(magic) {
            OpaqueValueError::InvalidFrameLength {
                opaque_type: STD_UI_TYPE_ID,
            }
        } else {
            OpaqueValueError::InvalidMagic {
                opaque_type: STD_UI_TYPE_ID,
            }
        });
    }
    let body_length = usize::try_from(u32::from_be_bytes(
        payload[magic.len()..prefix_length]
            .try_into()
            .expect("the UI length prefix is exactly four bytes"),
    ))
    .map_err(|_| OpaqueValueError::InvalidFrameLength {
        opaque_type: STD_UI_TYPE_ID,
    })?;
    let body_end =
        prefix_length
            .checked_add(body_length)
            .ok_or(OpaqueValueError::InvalidFrameLength {
                opaque_type: STD_UI_TYPE_ID,
            })?;
    if body_length > orna_core::value::MAX_OPAQUE_CODEC_PAYLOAD_LENGTH || body_end != payload.len()
    {
        return Err(OpaqueValueError::InvalidFrameLength {
            opaque_type: STD_UI_TYPE_ID,
        });
    }
    let body = &payload[prefix_length..body_end];
    let value = serde_json::from_slice(body).map_err(|_| OpaqueValueError::InvalidJsonBody {
        opaque_type: STD_UI_TYPE_ID,
    })?;
    let canonical = serde_json::to_vec(&value).map_err(|_| OpaqueValueError::InvalidJsonBody {
        opaque_type: STD_UI_TYPE_ID,
    })?;
    if canonical != body {
        return Err(OpaqueValueError::InvalidJsonBody {
            opaque_type: STD_UI_TYPE_ID,
        });
    }
    Ok(value)
}

pub(super) fn evaluate_standard_ui_constructor(
    active: &ActiveDatabaseRevision,
    context: ClientExecutionContext,
    spec: &StandardUiConstructorSpec,
    arguments: &[(ParameterId, RuntimeValue)],
) -> Result<RuntimeValue, Box<ClientExecutionError>> {
    let Some(standard) = active.catalogue_hash_context().standard() else {
        return Err(invalid_ui_constructor_value(
            context,
            OpaqueValueError::ActiveStandardRequired,
        ));
    };
    if !((standard.revision() == STANDARD_LIBRARY_V9_REVISION_ID
        && standard.catalogue().revision() == STANDARD_CATALOGUE_V9_REVISION_ID)
        || (standard.revision() == STANDARD_LIBRARY_V10_REVISION_ID
            && standard.catalogue().revision() == STANDARD_CATALOGUE_V10_REVISION_ID))
    {
        return Err(invalid_ui_constructor_registry(
            context,
            RegisteredOpaqueCodecsError::UnacceptedStandardSnapshot,
        ));
    }
    let registry = registered_opaque_codecs(standard)
        .map_err(|source| invalid_ui_constructor_registry(context, source))?;

    if arguments.len() != spec.parameters.len()
        || arguments
            .iter()
            .zip(spec.parameters)
            .any(|((parameter, _), (expected, _))| parameter != expected)
    {
        return Err(Box::new(expression_error(
            context,
            ClientExpressionError::InvalidCall,
        )));
    }
    if arguments
        .iter()
        .zip(spec.parameters)
        .any(|((_, value), (_, kind))| !ui_constructor_parameter_matches(value, *kind))
    {
        return Err(Box::new(expression_error(
            context,
            ClientExpressionError::TypeMismatch,
        )));
    }
    if arguments
        .iter()
        .zip(spec.parameters)
        .any(|((_, value), (_, kind))| {
            matches!(
                (kind, value),
                (
                    StandardUiConstructorParameterKind::Text,
                    RuntimeValue::Text(text)
                ) if text.len() > runtime_loader::CLIENT_MAX_RUNTIME_TEXT_BYTES
            )
        })
    {
        return Err(invalid_ui_constructor_value(
            context,
            OpaqueValueError::InvalidFrameLength {
                opaque_type: STD_UI_TYPE_ID,
            },
        ));
    }

    let mut properties = Map::new();
    let mut slots = Map::new();
    match spec.kind {
        StandardUiConstructorKind::Text => {
            let RuntimeValue::Text(text) = &arguments[0].1 else {
                unreachable!("constructor arguments were validated above");
            };
            properties.insert("text".to_owned(), ui_constructor_text_property(text));
        }
        StandardUiConstructorKind::Button => {
            let RuntimeValue::Text(label) = &arguments[0].1 else {
                unreachable!("constructor arguments were validated above");
            };
            let RuntimeValue::Boolean(enabled) = arguments[1].1 else {
                unreachable!("constructor arguments were validated above");
            };
            properties.insert("label".to_owned(), ui_constructor_text_property(label));
            properties.insert(
                "enabled".to_owned(),
                ui_constructor_boolean_property(enabled),
            );
        }
        StandardUiConstructorKind::TextInput => {
            let RuntimeValue::Text(text) = &arguments[0].1 else {
                unreachable!("constructor arguments were validated above");
            };
            let RuntimeValue::Text(placeholder) = &arguments[1].1 else {
                unreachable!("constructor arguments were validated above");
            };
            let RuntimeValue::Boolean(enabled) = arguments[2].1 else {
                unreachable!("constructor arguments were validated above");
            };
            properties.insert("text".to_owned(), ui_constructor_text_property(text));
            properties.insert(
                "placeholder".to_owned(),
                ui_constructor_text_property(placeholder),
            );
            properties.insert(
                "enabled".to_owned(),
                ui_constructor_boolean_property(enabled),
            );
        }
        StandardUiConstructorKind::Panel
        | StandardUiConstructorKind::Row
        | StandardUiConstructorKind::Column
        | StandardUiConstructorKind::Tabs => {
            let RuntimeValue::Opaque(content) = &arguments[0].1 else {
                unreachable!("constructor arguments were validated above");
            };
            let content = OpaqueValue::new(
                active,
                &registry,
                STD_UI_TYPE_ID,
                content.canonical_payload(),
            )
            .map_err(|source| invalid_ui_constructor_value(context, source))?;
            let content = decode_ui_constructor_body(content.canonical_payload())
                .map_err(|source| invalid_ui_constructor_value(context, source))?;
            slots.insert("content".to_owned(), Value::Array(vec![content]));
        }
    }

    let mut node = Map::new();
    node.insert("kind".to_owned(), Value::String("node".to_owned()));
    let mut contract = Map::new();
    contract.insert(
        "id".to_owned(),
        Value::String(spec.node_contract.to_owned()),
    );
    contract.insert(
        "name".to_owned(),
        Value::String(spec.node_contract.to_owned()),
    );
    contract.insert("version".to_owned(), Value::String("1.0".to_owned()));
    node.insert("contract".to_owned(), Value::Object(contract));
    node.insert("properties".to_owned(), Value::Object(properties));
    node.insert("slots".to_owned(), Value::Object(slots));
    node.insert("actions".to_owned(), Value::Object(Map::new()));
    let body = serde_json::to_vec(&Value::Object(node)).map_err(|_| {
        invalid_ui_constructor_value(
            context,
            OpaqueValueError::InvalidJsonBody {
                opaque_type: STD_UI_TYPE_ID,
            },
        )
    })?;
    let body_length = u32::try_from(body.len()).map_err(|_| {
        invalid_ui_constructor_value(
            context,
            OpaqueValueError::InvalidFrameLength {
                opaque_type: STD_UI_TYPE_ID,
            },
        )
    })?;
    if body.len() > orna_core::value::MAX_OPAQUE_CODEC_PAYLOAD_LENGTH {
        return Err(invalid_ui_constructor_value(
            context,
            OpaqueValueError::InvalidFrameLength {
                opaque_type: STD_UI_TYPE_ID,
            },
        ));
    }
    let payload_capacity = UI_MAGIC
        .len()
        .checked_add(4)
        .and_then(|length| length.checked_add(body.len()))
        .ok_or_else(|| {
            invalid_ui_constructor_value(
                context,
                OpaqueValueError::InvalidFrameLength {
                    opaque_type: STD_UI_TYPE_ID,
                },
            )
        })?;
    let mut payload = Vec::with_capacity(payload_capacity);
    payload.extend_from_slice(UI_MAGIC.as_bytes());
    payload.extend_from_slice(&body_length.to_be_bytes());
    payload.extend_from_slice(&body);
    let value = OpaqueValue::new(active, &registry, STD_UI_TYPE_ID, payload)
        .map_err(|source| invalid_ui_constructor_value(context, source))?;
    Ok(RuntimeValue::Opaque(value))
}
