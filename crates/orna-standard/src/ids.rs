//! Stable standard-library identities consumed by the runtime and clients.
//!
//! These values are protocol facts and do not depend on a source parser.

use orna_core::{
    FunctionId, FunctionRevisionId, ParameterId, SchemaId, SourceUnitId, TypeBindingId, TypeId,
};

/// The fixed Work ADR 0087 `std/data.orna` source-unit identity: `...09`.
pub const STD_DATA_SOURCE_UNIT_ID: SourceUnitId =
    SourceUnitId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x09]);

/// The deterministic `std.rows` qualified type-binding identity (derived by
/// `TypeBinding::qualified` from the normalised unquoted source spelling
/// `std.Rows`).
pub const STD_DATA_ROWS_TYPE_BINDING_ID: TypeBindingId = TypeBindingId::from_bytes([
    0x04, 0xe2, 0x43, 0x98, 0x0b, 0x43, 0xc2, 0xaa, 0xa0, 0x0e, 0x0e, 0x79, 0xc4, 0xce, 0xea, 0x10,
]);

/// The fixed ADR 0019 `std.ui.window` function identity: `...14`.
pub const STD_UI_WINDOW_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x14]);

/// The fixed ADR 0019 `std.ui.window.title` parameter identity: `...14`.
pub const STD_UI_WINDOW_TITLE_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x14]);

/// The fixed ADR 0019 `std.ui.window.content` parameter identity: `...15`.
pub const STD_UI_WINDOW_CONTENT_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x15]);

/// The fixed ADR 0019 `std.ui.window` function-revision identity: `...14`.
pub const STD_UI_WINDOW_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x14]);

/// The `std.ui.window` function revision number.
pub const STD_UI_WINDOW_REVISION_NUMBER: u64 = 1;

/// The fixed ADR 0019 runtime contract identity.
pub const STD_UI_WINDOW_RUNTIME_CONTRACT: &str = "std.ui.window@1";

/// The fixed Work ADR 0088 `std.ui.text` function identity: `...15`.
pub const STD_UI_TEXT_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x15]);

/// The fixed Work ADR 0088 `std.ui.text.text` parameter identity: `...16`.
pub const STD_UI_TEXT_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x16]);

/// The fixed Work ADR 0088 `std.ui.text` function-revision identity: `...15`.
pub const STD_UI_TEXT_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x15]);

/// The fixed Work ADR 0088 `std.ui.text@1` external contract.
pub const STD_UI_TEXT_RUNTIME_CONTRACT: &str = "std.ui.text@1";

/// The fixed Work ADR 0088 `std.ui.button` function identity: `...16`.
pub const STD_UI_BUTTON_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x16]);

/// The fixed Work ADR 0088 `std.ui.button.label` parameter identity: `...17`.
pub const STD_UI_BUTTON_LABEL_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x17]);

/// The fixed Work ADR 0088 `std.ui.button.enabled` parameter identity: `...18`.
pub const STD_UI_BUTTON_ENABLED_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x18]);

/// The fixed Work ADR 0088 `std.ui.button` function-revision identity: `...16`.
pub const STD_UI_BUTTON_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x16]);

/// The fixed Work ADR 0088 `std.ui.button@1` external contract.
pub const STD_UI_BUTTON_RUNTIME_CONTRACT: &str = "std.ui.button@1";

/// The fixed Work ADR 0088 `std.ui.panel` function identity: `...17`.
pub const STD_UI_PANEL_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x17]);

/// The fixed Work ADR 0088 `std.ui.panel.content` parameter identity: `...19`.
pub const STD_UI_PANEL_CONTENT_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x19]);

/// The fixed Work ADR 0088 `std.ui.panel` function-revision identity: `...17`.
pub const STD_UI_PANEL_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x17]);

/// The fixed Work ADR 0088 `std.ui.panel@1` external contract.
pub const STD_UI_PANEL_RUNTIME_CONTRACT: &str = "std.ui.panel@1";

/// The fixed Work ADR 0088 `std.ui.row` function identity: `...18`.
pub const STD_UI_ROW_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x18]);

/// The fixed Work ADR 0088 `std.ui.row.content` parameter identity: `...1A`.
pub const STD_UI_ROW_CONTENT_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1A]);

/// The fixed Work ADR 0088 `std.ui.row` function-revision identity: `...18`.
pub const STD_UI_ROW_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x18]);

/// The fixed Work ADR 0088 `std.ui.row@1` external contract.
pub const STD_UI_ROW_RUNTIME_CONTRACT: &str = "std.ui.row@1";

/// The fixed Work ADR 0088 `std.ui.column` function identity: `...19`.
pub const STD_UI_COLUMN_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x19]);

/// The fixed Work ADR 0088 `std.ui.column.content` parameter identity: `...1B`.
pub const STD_UI_COLUMN_CONTENT_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1B]);

/// The fixed Work ADR 0088 `std.ui.column` function-revision identity: `...19`.
pub const STD_UI_COLUMN_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x19]);

/// The fixed Work ADR 0088 `std.ui.column@1` external contract.
pub const STD_UI_COLUMN_RUNTIME_CONTRACT: &str = "std.ui.column@1";

/// The fixed Work ADR 0088 `std.ui.text_input` function identity: `...1A`.
pub const STD_UI_TEXT_INPUT_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1A]);

/// The fixed Work ADR 0088 `std.ui.text_input.text` parameter identity: `...1C`.
pub const STD_UI_TEXT_INPUT_TEXT_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1C]);

/// The fixed Work ADR 0088 `std.ui.text_input.placeholder` parameter identity: `...1D`.
pub const STD_UI_TEXT_INPUT_PLACEHOLDER_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1D]);

/// The fixed Work ADR 0088 `std.ui.text_input.enabled` parameter identity: `...1E`.
pub const STD_UI_TEXT_INPUT_ENABLED_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1E]);

/// The fixed Work ADR 0088 `std.ui.text_input` function-revision identity: `...1A`.
pub const STD_UI_TEXT_INPUT_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1A]);

/// The fixed Work ADR 0088 `std.ui.text_input@1` external contract.
pub const STD_UI_TEXT_INPUT_RUNTIME_CONTRACT: &str = "std.ui.text_input@1";

/// The fixed Work ADR 0088 `std.ui.tabs` function identity: `...1B`.
pub const STD_UI_TABS_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1B]);

/// The fixed Work ADR 0088 `std.ui.tabs.content` parameter identity: `...1F`.
pub const STD_UI_TABS_CONTENT_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1F]);

/// The fixed Work ADR 0088 `std.ui.tabs` function-revision identity: `...1B`.
pub const STD_UI_TABS_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x1B]);

/// The fixed Work ADR 0088 `std.ui.tabs@1` external contract.
pub const STD_UI_TABS_RUNTIME_CONTRACT: &str = "std.ui.tabs@1";

/// The fixed ADR 0055 `std.invoke` schema identity: 15 zero bytes then `0x03`.
pub const STD_INVOKE_SCHEMA_ID: SchemaId =
    SchemaId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x03]);

/// The fixed ADR 0055 `std.invoke.echo` function identity: 15 zero bytes then `0x10`.
pub const STD_INVOKE_ECHO_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10]);

/// The fixed ADR 0055 `std.invoke.echo.p_value` parameter identity: `...10`.
pub const STD_INVOKE_ECHO_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10]);

/// The fixed ADR 0055 `std.invoke.echo` function-revision identity: `...10`.
pub const STD_INVOKE_ECHO_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10]);

/// The `std.invoke.echo` revision number: version 1 (ADR 0055).
pub const STD_INVOKE_ECHO_REVISION_NUMBER: u64 = 1;

/// The fixed ADR 0055 `std/types.orna` source-unit identity: `...02`.
pub const STD_TYPES_SOURCE_UNIT_ID: SourceUnitId =
    SourceUnitId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x02]);

/// The fixed ADR 0055 `std/invoke.orna` source-unit identity: `...03`.
pub const STD_INVOKE_SOURCE_UNIT_ID: SourceUnitId =
    SourceUnitId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x03]);

/// The fixed ADR 0055 INTEGER value-type identity: `...02`.
pub const STD_INTEGER_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x02]);

/// The fixed V5/ADR 0075 `std.json` schema identity: 15 zero bytes then `0x06`.
pub const STD_JSON_SCHEMA_ID: SchemaId =
    SchemaId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x06]);

/// The reserved, unregistered `std.data` schema identity: 15 zero bytes then `0x07`.
pub const STD_DATA_SCHEMA_ID: SchemaId =
    SchemaId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x07]);

/// The `std.json.Value` value type introduced by V5/ADR 0075 has identity `...11`.
pub const STD_JSON_VALUE_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11]);

/// The reserved, unregistered `std.data.Rows` value-type identity: `...12`.
pub const STD_DATA_ROWS_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x12]);

/// The fixed ADR 0057 `std.json.encode` function identity retained by V5/ADR 0075:
/// 15 zero bytes then `0x11`.
pub const STD_JSON_ENCODE_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11]);

/// The fixed ADR 0057 `std.json.encode.p_value` parameter identity retained by
/// V5/ADR 0075: `...11`.
pub const STD_JSON_ENCODE_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11]);

/// The fixed ADR 0057 `std.json.encode` function-revision identity retained by
/// V5/ADR 0075: `...11`.
pub const STD_JSON_ENCODE_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x11]);

/// The fixed ADR 0057 `std.terminal.present_table` function identity: `...12`.
pub const STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x12]);

/// The fixed ADR 0057 `std.terminal.present_table.p_rows` parameter identity: `...12`.
pub const STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID: ParameterId =
    ParameterId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x12]);

/// The fixed ADR 0057 `std.terminal.present_table` function-revision identity: `...12`.
pub const STD_TERMINAL_PRESENT_TABLE_FUNCTION_REVISION_ID: FunctionRevisionId =
    FunctionRevisionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x12]);
