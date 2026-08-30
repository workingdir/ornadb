//! Retained standard-source reconciliation and snapshot construction.

use super::*;
pub(super) fn reconcile_retained_data_source(
    source: &str,
    catalogue: &CatalogueSnapshot,
) -> Result<Vec<DefinitionOrigin>, StandardLibraryError> {
    let parsed = orna_syntax::parse(source);
    if !parsed.diagnostics().is_empty()
        || parsed.syntax().text() != source
        || !parsed.object_types().is_empty()
        || !parsed.field_renames().is_empty()
        || !parsed.client_functions().is_empty()
        || !parsed.primitive_value_types().is_empty()
        || !parsed.record_value_types().is_empty()
        || !parsed.enum_types().is_empty()
        || parsed.schemas().len() != 1
        || parsed.opaque_value_types().len() != 1
        || parsed.type_exports().len() != 1
        || parsed.server_functions().len() != 1
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let schema = catalogue
        .schema_by_id(STD_DATA_SCHEMA_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let rows_definition = catalogue
        .type_definition_by_id(STD_DATA_ROWS_TYPE_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?
        .as_opaque_value()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let rows_binding = catalogue
        .type_binding_by_id(STD_DATA_ROWS_TYPE_BINDING_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let declaration_schema = &parsed.schemas()[0];
    let declaration_type = &parsed.opaque_value_types()[0];
    let declaration_export = &parsed.type_exports()[0];
    let [declaration_function] = parsed.server_functions() else {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    };
    if !matches_qualified_name(&declaration_schema.name, schema.name())
        || !matches_qualified_name(&declaration_type.name, rows_definition.name())
        || decode_sql_string_literal(&declaration_type.kernel_contract.text).as_deref()
            != Some(rows_definition.representation_contract())
        || rows_definition.mutability() != ValueTypeMutability::Immutable
        || rows_definition.persistence() != ValueTypePersistence::Transient
        || !matches_qualified_export(
            declaration_export,
            rows_definition.name(),
            STD_DATA_ROWS_TYPE_ID,
            rows_binding,
        )
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    if !matches!(
        &declaration_function.name.parts[..],
        [first, second, third]
            if is_unquoted(first)
                && is_unquoted(second)
                && is_unquoted(third)
                && first.text.eq_ignore_ascii_case("std")
                && second.text.eq_ignore_ascii_case("terminal")
                && third.text.eq_ignore_ascii_case("present_table")
    ) {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let parameter = declaration_function
        .parameters
        .first()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let origin = |span: &orna_syntax::SourceSpan, identity| {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end =
            u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        Ok(DefinitionOrigin::new(
            identity,
            SourceOrigin::new(STD_DATA_SOURCE_UNIT_ID, start, end)
                .map_err(|source| StandardLibraryError::Revision { source })?,
        ))
    };
    let mut origins = vec![
        origin(
            &declaration_schema.span,
            DefinitionIdentity::Schema(STD_DATA_SCHEMA_ID),
        )?,
        origin(
            &declaration_type.span,
            DefinitionIdentity::ValueType(STD_DATA_ROWS_TYPE_ID),
        )?,
        origin(
            &declaration_export.span,
            DefinitionIdentity::TypeBinding(rows_binding.id()),
        )?,
        origin(
            &declaration_function.span,
            DefinitionIdentity::Function(STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID),
        )?,
        origin(
            &parameter.span,
            DefinitionIdentity::Parameter {
                owner: STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
                parameter: STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
            },
        )?,
    ];
    origins.sort_by_key(|origin| (origin.source().byte_start(), origin.source().byte_end()));
    orna_compiler::check_standard_terminal_present_table(
        declaration_function,
        catalogue,
        &origins,
        STD_DATA_ROWS_TYPE_ID,
    )
    .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
    Ok(origins)
}

pub(super) fn reconcile_retained_window_source(
    source: &str,
    catalogue: &CatalogueSnapshot,
) -> Result<Vec<DefinitionOrigin>, StandardLibraryError> {
    let parsed = orna_syntax::parse(source);
    if !parsed.diagnostics().is_empty()
        || parsed.syntax().text() != source
        || !parsed.schemas().is_empty()
        || !parsed.object_types().is_empty()
        || !parsed.field_renames().is_empty()
        || !parsed.server_functions().is_empty()
        || parsed.client_functions().len() != 1
        || !parsed.primitive_value_types().is_empty()
        || !parsed.opaque_value_types().is_empty()
        || !parsed.record_value_types().is_empty()
        || !parsed.enum_types().is_empty()
        || !parsed.type_exports().is_empty()
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let [function] = parsed.client_functions() else {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    };
    let origin = |span: &orna_syntax::SourceSpan, identity| {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end =
            u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        Ok(DefinitionOrigin::new(
            identity,
            SourceOrigin::new(STD_WINDOW_SOURCE_UNIT_ID, start, end)
                .map_err(|source| StandardLibraryError::Revision { source })?,
        ))
    };
    let [title, content] = function.parameters.as_slice() else {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    };
    let mut origins = vec![
        origin(
            &function.span,
            DefinitionIdentity::Function(STD_UI_WINDOW_FUNCTION_ID),
        )?,
        origin(
            &title.span,
            DefinitionIdentity::Parameter {
                owner: STD_UI_WINDOW_FUNCTION_ID,
                parameter: STD_UI_WINDOW_TITLE_PARAMETER_ID,
            },
        )?,
        origin(
            &content.span,
            DefinitionIdentity::Parameter {
                owner: STD_UI_WINDOW_FUNCTION_ID,
                parameter: STD_UI_WINDOW_CONTENT_PARAMETER_ID,
            },
        )?,
    ];
    origins.sort_by_key(|origin| (origin.source().byte_start(), origin.source().byte_end()));
    orna_compiler::check_standard_ui_window(function, catalogue, &origins)
        .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
    Ok(origins)
}

pub(super) fn reconcile_retained_ui_constructors_source(
    source: &str,
    catalogue: &CatalogueSnapshot,
) -> Result<Vec<DefinitionOrigin>, StandardLibraryError> {
    let parsed = orna_syntax::parse(source);
    if !parsed.diagnostics().is_empty()
        || parsed.syntax().text() != source
        || !parsed.schemas().is_empty()
        || !parsed.object_types().is_empty()
        || !parsed.field_renames().is_empty()
        || !parsed.server_functions().is_empty()
        || !parsed.primitive_value_types().is_empty()
        || !parsed.opaque_value_types().is_empty()
        || !parsed.record_value_types().is_empty()
        || !parsed.enum_types().is_empty()
        || !parsed.type_exports().is_empty()
        || parsed.client_functions().len() != 7
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let expected_names = [
        "text",
        "button",
        "panel",
        "row",
        "column",
        "text_input",
        "tabs",
    ];
    let expected_functions = [
        STD_UI_TEXT_FUNCTION_ID,
        STD_UI_BUTTON_FUNCTION_ID,
        STD_UI_PANEL_FUNCTION_ID,
        STD_UI_ROW_FUNCTION_ID,
        STD_UI_COLUMN_FUNCTION_ID,
        STD_UI_TEXT_INPUT_FUNCTION_ID,
        STD_UI_TABS_FUNCTION_ID,
    ];
    let expected_parameters: [&[orna_core::ParameterId]; 7] = [
        &[STD_UI_TEXT_PARAMETER_ID],
        &[
            STD_UI_BUTTON_LABEL_PARAMETER_ID,
            STD_UI_BUTTON_ENABLED_PARAMETER_ID,
        ],
        &[STD_UI_PANEL_CONTENT_PARAMETER_ID],
        &[STD_UI_ROW_CONTENT_PARAMETER_ID],
        &[STD_UI_COLUMN_CONTENT_PARAMETER_ID],
        &[
            STD_UI_TEXT_INPUT_TEXT_PARAMETER_ID,
            STD_UI_TEXT_INPUT_PLACEHOLDER_PARAMETER_ID,
            STD_UI_TEXT_INPUT_ENABLED_PARAMETER_ID,
        ],
        &[STD_UI_TABS_CONTENT_PARAMETER_ID],
    ];
    let origin = |span: &orna_syntax::SourceSpan, identity| {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end =
            u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        Ok(DefinitionOrigin::new(
            identity,
            SourceOrigin::new(STD_UI_CONSTRUCTORS_SOURCE_UNIT_ID, start, end)
                .map_err(|source| StandardLibraryError::Revision { source })?,
        ))
    };
    let mut origins = Vec::new();
    let mut function_origins = Vec::with_capacity(7);
    for (index, function) in parsed.client_functions().iter().enumerate() {
        let [first, second, third] = function.name.parts.as_slice() else {
            return Err(StandardLibraryError::RetainedSourceMismatch);
        };
        if !is_unquoted(first)
            || !is_unquoted(second)
            || !is_unquoted(third)
            || !first.text.eq_ignore_ascii_case("std")
            || !second.text.eq_ignore_ascii_case("ui")
            || !third.text.eq_ignore_ascii_case(expected_names[index])
        {
            return Err(StandardLibraryError::RetainedSourceMismatch);
        }
        if function.parameters.len() != expected_parameters[index].len() {
            return Err(StandardLibraryError::RetainedSourceMismatch);
        }
        let function_origin = origin(
            &function.span,
            DefinitionIdentity::Function(expected_functions[index]),
        )?;
        let mut group = vec![function_origin.clone()];
        for (parameter, parameter_id) in function.parameters.iter().zip(expected_parameters[index])
        {
            group.push(origin(
                &parameter.span,
                DefinitionIdentity::Parameter {
                    owner: expected_functions[index],
                    parameter: *parameter_id,
                },
            )?);
        }
        origins.extend(group.iter().cloned());
        function_origins.push(group);
    }
    origins.sort_by_key(|origin| (origin.source().byte_start(), origin.source().byte_end()));
    for (function, group) in parsed
        .client_functions()
        .iter()
        .zip(function_origins.iter())
    {
        orna_compiler::check_standard_ui_constructor(function, catalogue, group)
            .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
    }
    Ok(origins)
}

pub(super) fn reconcile_retained_action_source(
    source: &str,
    catalogue: &CatalogueSnapshot,
) -> Result<Vec<DefinitionOrigin>, StandardLibraryError> {
    let parsed = orna_syntax::parse(source);
    if !parsed.diagnostics().is_empty()
        || parsed.syntax().text() != source
        || !parsed.object_types().is_empty()
        || !parsed.field_renames().is_empty()
        || !parsed.server_functions().is_empty()
        || !parsed.client_functions().is_empty()
        || !parsed.primitive_value_types().is_empty()
        || !parsed.record_value_types().is_empty()
        || !parsed.enum_types().is_empty()
        || parsed.schemas().len() != 1
        || parsed.opaque_value_types().len() != 1
        || parsed.type_exports().len() != 1
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let action_schema = catalogue
        .schema_by_id(STD_ACTION_SCHEMA_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    if !matches_qualified_name(&parsed.schemas()[0].name, action_schema.name()) {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let action_definition = catalogue
        .type_definition_by_id(STD_ACTION_TYPE_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?
        .as_opaque_value()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let declaration = &parsed.opaque_value_types()[0];
    if !matches_qualified_name(&declaration.name, action_definition.name())
        || decode_sql_string_literal(&declaration.kernel_contract.text).as_deref()
            != Some(action_definition.representation_contract())
        || action_definition.persistence() != ValueTypePersistence::Transient
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let action_binding = catalogue
        .type_binding_by_name(&TypeLookupName::qualified(
            semantic_name("std.action", ["std", "action"])
                .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?,
        ))
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    if !matches_qualified_export(
        &parsed.type_exports()[0],
        action_definition.name(),
        STD_ACTION_TYPE_ID,
        action_binding,
    ) {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let origin = |span: &orna_syntax::SourceSpan| -> Result<SourceOrigin, StandardLibraryError> {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end =
            u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        SourceOrigin::new(STD_ACTION_SOURCE_UNIT_ID, start, end)
            .map_err(|source| StandardLibraryError::Revision { source })
    };
    let mut declarations = vec![
        (
            parsed.schemas()[0].span.clone(),
            DefinitionIdentity::Schema(STD_ACTION_SCHEMA_ID),
        ),
        (
            parsed.opaque_value_types()[0].span.clone(),
            DefinitionIdentity::ValueType(STD_ACTION_TYPE_ID),
        ),
        (
            parsed.type_exports()[0].span.clone(),
            DefinitionIdentity::TypeBinding(action_binding.id()),
        ),
    ];
    declarations.sort_by_key(|(span, _)| span.start);
    let expected_identities = [
        DefinitionIdentity::Schema(STD_ACTION_SCHEMA_ID),
        DefinitionIdentity::ValueType(STD_ACTION_TYPE_ID),
        DefinitionIdentity::TypeBinding(action_binding.id()),
    ];
    if declarations
        .iter()
        .map(|(_, identity)| *identity)
        .ne(expected_identities.iter().copied())
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    declarations
        .into_iter()
        .map(|(span, identity)| Ok(DefinitionOrigin::new(identity, origin(&span)?)))
        .collect()
}

pub(super) fn reconcile_retained_json_source(
    source: &str,
    catalogue: &CatalogueSnapshot,
) -> Result<Vec<DefinitionOrigin>, StandardLibraryError> {
    let parsed = orna_syntax::parse(source);
    if !parsed.diagnostics().is_empty()
        || parsed.syntax().text() != source
        || !parsed.object_types().is_empty()
        || !parsed.field_renames().is_empty()
        || parsed.server_functions().len() != 1
        || !parsed.client_functions().is_empty()
        || !parsed.primitive_value_types().is_empty()
        || !parsed.record_value_types().is_empty()
        || !parsed.enum_types().is_empty()
        || parsed.schemas().len() != 1
        || parsed.opaque_value_types().len() != 1
        || parsed.type_exports().len() != 1
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let schema = catalogue
        .schema_by_id(STD_JSON_SCHEMA_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let value = catalogue
        .type_definition_by_id(STD_JSON_VALUE_TYPE_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?
        .as_opaque_value()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let binding = catalogue
        .type_binding_by_name(&TypeLookupName::qualified(
            semantic_name("std.jsonvalue", ["std", "jsonvalue"])
                .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?,
        ))
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    if !matches_qualified_name(&parsed.schemas()[0].name, schema.name())
        || !matches_qualified_name(&parsed.opaque_value_types()[0].name, value.name())
        || decode_sql_string_literal(&parsed.opaque_value_types()[0].kernel_contract.text)
            .as_deref()
            != Some(value.representation_contract())
        || value.persistence() != ValueTypePersistence::Transient
        || !matches_qualified_export(
            &parsed.type_exports()[0],
            value.name(),
            STD_JSON_VALUE_TYPE_ID,
            binding,
        )
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let function = &parsed.server_functions()[0];
    if function
        .name
        .parts
        .iter()
        .map(|part| part.text.as_str())
        .collect::<Vec<_>>()
        != ["std", "json", "encode"]
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let function_definition = catalogue
        .function_by_id(STD_JSON_ENCODE_FUNCTION_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    if function_definition.name()
        != &semantic_name("std.json.encode", ["std", "json", "encode"])
            .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let parameter = function
        .parameters
        .first()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let origin = |span: &orna_syntax::SourceSpan, identity| {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end =
            u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        Ok(DefinitionOrigin::new(
            identity,
            SourceOrigin::new(STD_JSON_SOURCE_UNIT_ID, start, end)
                .map_err(|source| StandardLibraryError::Revision { source })?,
        ))
    };
    let mut declarations = vec![
        origin(
            &parsed.schemas()[0].span,
            DefinitionIdentity::Schema(STD_JSON_SCHEMA_ID),
        )?,
        origin(
            &parsed.opaque_value_types()[0].span,
            DefinitionIdentity::ValueType(STD_JSON_VALUE_TYPE_ID),
        )?,
        origin(
            &parsed.type_exports()[0].span,
            DefinitionIdentity::TypeBinding(binding.id()),
        )?,
        origin(
            &function.span,
            DefinitionIdentity::Function(STD_JSON_ENCODE_FUNCTION_ID),
        )?,
        origin(
            &parameter.span,
            DefinitionIdentity::Parameter {
                owner: STD_JSON_ENCODE_FUNCTION_ID,
                parameter: STD_JSON_ENCODE_PARAMETER_ID,
            },
        )?,
    ];
    declarations.sort_by_key(|origin| (origin.source().byte_start(), origin.source().byte_end()));
    orna_compiler::check_standard_json_encode(
        function,
        catalogue,
        &declarations,
        STD_JSON_VALUE_TYPE_ID,
    )
    .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
    Ok(declarations)
}

/// Reconciles the retained `std/ui.orna` unit against the V4 catalogue.
///
/// The unit must round-trip exactly and contain nothing besides the
/// `std.ui` schema declaration, the single opaque UI value type declaration
/// (`std.ui.UI` with the `orna.std.value.ui@1` kernel contract and the
/// `IMMUTABLE TRANSIENT` catalogue facts), and the single `std.ui` qualified
/// export. The complete origin set is exactly those three declarations at
/// their exact byte ranges in the retained unit.
pub(super) fn reconcile_retained_ui_source(
    source: &str,
    catalogue: &CatalogueSnapshot,
) -> Result<Vec<DefinitionOrigin>, StandardLibraryError> {
    let parsed = orna_syntax::parse(source);
    if !parsed.diagnostics().is_empty()
        || parsed.syntax().text() != source
        || !parsed.object_types().is_empty()
        || !parsed.field_renames().is_empty()
        || !parsed.server_functions().is_empty()
        || !parsed.client_functions().is_empty()
        || !parsed.primitive_value_types().is_empty()
        || !parsed.record_value_types().is_empty()
        || !parsed.enum_types().is_empty()
        || parsed.schemas().len() != 1
        || parsed.opaque_value_types().len() != 1
        || parsed.type_exports().len() != 1
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    let ui_schema = catalogue
        .schema_by_id(STD_UI_SCHEMA_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    if !matches_qualified_name(&parsed.schemas()[0].name, ui_schema.name()) {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    let ui_definition = catalogue
        .type_definition_by_id(STD_UI_TYPE_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?
        .as_opaque_value()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let declaration = &parsed.opaque_value_types()[0];
    if !matches_qualified_name(&declaration.name, ui_definition.name())
        || decode_sql_string_literal(&declaration.kernel_contract.text).as_deref()
            != Some(ui_definition.representation_contract())
        || !matches!(ui_definition.mutability(), ValueTypeMutability::Immutable)
        || ui_definition.persistence() != ValueTypePersistence::Transient
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    let ui_binding = catalogue
        .type_bindings()
        .get(33)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    if !matches_qualified_export(
        &parsed.type_exports()[0],
        ui_definition.name(),
        STD_UI_TYPE_ID,
        ui_binding,
    ) {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    let origin = |span: &orna_syntax::SourceSpan| -> Result<SourceOrigin, StandardLibraryError> {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end =
            u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        SourceOrigin::new(STD_UI_SOURCE_UNIT_ID, start, end)
            .map_err(|source| StandardLibraryError::Revision { source })
    };

    let expected_identities = [
        DefinitionIdentity::Schema(STD_UI_SCHEMA_ID),
        DefinitionIdentity::ValueType(STD_UI_TYPE_ID),
        DefinitionIdentity::TypeBinding(ui_binding.id()),
    ];
    let mut declarations = vec![
        (
            parsed.schemas()[0].span.clone(),
            DefinitionIdentity::Schema(STD_UI_SCHEMA_ID),
        ),
        (
            parsed.opaque_value_types()[0].span.clone(),
            DefinitionIdentity::ValueType(STD_UI_TYPE_ID),
        ),
        (
            parsed.type_exports()[0].span.clone(),
            DefinitionIdentity::TypeBinding(ui_binding.id()),
        ),
    ];
    declarations.sort_by_key(|(span, _)| span.start);
    if declarations
        .iter()
        .map(|(_, identity)| *identity)
        .ne(expected_identities.iter().copied())
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    declarations
        .into_iter()
        .map(|(span, identity)| Ok(DefinitionOrigin::new(identity, origin(&span)?)))
        .collect()
}

pub(super) fn retained_standard_library_v3_snapshot_from_source(
    types_source: &str,
    invoke_source: &str,
    output_source: &str,
) -> Result<StandardLibrarySnapshot, StandardLibraryError> {
    let manifest = standard_library_v3_manifest()
        .map_err(|source| StandardLibraryError::Manifest { source })?;
    let types_manifest =
        standard_library_manifest().map_err(|source| StandardLibraryError::Manifest { source })?;
    let catalogue = manifest.catalogue();

    let mut origins = reconcile_retained_source_with_unit(
        types_source,
        &types_manifest,
        STD_TYPES_SOURCE_UNIT_ID,
    )?;
    let invoke_origins = reconcile_retained_invoke_source(invoke_source, catalogue)?;
    origins.extend(invoke_origins.iter().cloned());
    let output_origins = reconcile_retained_output_source(output_source, catalogue)?;
    origins.extend(output_origins.iter().cloned());

    let types_content_hash = source_unit_content_digest(types_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if types_content_hash != ACCEPTED_V3_TYPES_CONTENT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let invoke_content_hash = source_unit_content_digest(invoke_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if invoke_content_hash != ACCEPTED_V3_INVOKE_CONTENT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let output_content_hash = source_unit_content_digest(output_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if output_content_hash != ACCEPTED_V3_OUTPUT_CONTENT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let types_unit = StoredSourceUnit::new(
        STD_TYPES_SOURCE_UNIT_ID,
        0,
        SOURCE_LOGICAL_PATH,
        types_source,
        types_content_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let invoke_unit = StoredSourceUnit::new(
        STD_INVOKE_SOURCE_UNIT_ID,
        1,
        STD_INVOKE_SOURCE_LOGICAL_PATH,
        invoke_source,
        invoke_content_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let output_unit = StoredSourceUnit::new(
        STD_OUTPUT_SOURCE_UNIT_ID,
        2,
        STD_OUTPUT_SOURCE_LOGICAL_PATH,
        output_source,
        output_content_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let units = vec![types_unit, invoke_unit, output_unit];
    let bundle_hash = source_bundle_digest(&units)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if bundle_hash != ACCEPTED_V3_SOURCE_BUNDLE_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let revision_hash = source_revision_record_digest(
        STANDARD_SOURCE_V3_BUNDLE_ID,
        Some(STANDARD_SOURCE_V2_REVISION_ID),
        bundle_hash,
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if revision_hash != ACCEPTED_V3_SOURCE_REVISION_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let retained_source = StoredSourceRevision::new(
        STANDARD_SOURCE_V3_BUNDLE_ID,
        STANDARD_SOURCE_V3_REVISION_ID,
        Some(STANDARD_SOURCE_V2_REVISION_ID),
        units,
        bundle_hash,
        revision_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;

    // `orna.std/3` retains the exact V2 parameter-echo executable unchanged;
    // its artifact and semantic digests are the V2 goldens, pinned here as the
    // V3 goldens so the retained path fails closed on any drift.
    let executable = retained_v2_executable(invoke_source, catalogue, &invoke_origins)?;
    if executable.revision().artifact().content_hash() != ACCEPTED_V3_ARTIFACT_DIGEST
        || executable.revision().semantic_hash() != ACCEPTED_V3_SEMANTIC_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let snapshot = StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V3_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        retained_source,
        LANGUAGE_VERSION_IDENTITY,
        catalogue.clone(),
        vec![executable],
        origins,
        ACCEPTED_V3_STANDARD_LIBRARY_DIGEST,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let _ = standard_library_digest(&snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;

    Ok(snapshot)
}

/// Reconciles the retained `std/output.orna` unit against the V3 catalogue.
///
/// The unit must round-trip exactly and contain nothing besides the
/// `std.terminal` and `std.io` schema declarations, the two opaque output
/// value type declarations, and their two qualified exports. The complete
/// origin set is exactly those six declarations at their exact byte ranges in
/// the retained unit.
pub(super) fn reconcile_retained_output_source(
    source: &str,
    catalogue: &CatalogueSnapshot,
) -> Result<Vec<DefinitionOrigin>, StandardLibraryError> {
    let parsed = orna_syntax::parse(source);
    if !parsed.diagnostics().is_empty()
        || parsed.syntax().text() != source
        || !parsed.object_types().is_empty()
        || !parsed.field_renames().is_empty()
        || !parsed.server_functions().is_empty()
        || !parsed.client_functions().is_empty()
        || !parsed.primitive_value_types().is_empty()
        || !parsed.record_value_types().is_empty()
        || !parsed.enum_types().is_empty()
        || parsed.schemas().len() != 2
        || parsed.opaque_value_types().len() != 2
        || parsed.type_exports().len() != 2
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    let terminal_schema = catalogue
        .schema_by_id(STD_TERMINAL_SCHEMA_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let io_schema = catalogue
        .schema_by_id(STD_IO_SCHEMA_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    if !matches_qualified_name(&parsed.schemas()[0].name, terminal_schema.name())
        || !matches_qualified_name(&parsed.schemas()[1].name, io_schema.name())
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    let document_definition = catalogue
        .type_definition_by_id(STD_TERMINAL_DOCUMENT_TYPE_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?
        .as_opaque_value()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let bytestream_definition = catalogue
        .type_definition_by_id(STD_IO_BYTE_STREAM_TYPE_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?
        .as_opaque_value()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let output_definitions = [document_definition, bytestream_definition];
    for (declaration, definition) in parsed.opaque_value_types().iter().zip(output_definitions) {
        if !matches_qualified_name(&declaration.name, definition.name())
            || decode_sql_string_literal(&declaration.kernel_contract.text).as_deref()
                != Some(definition.representation_contract())
            || definition.persistence() != ValueTypePersistence::Transient
        {
            return Err(StandardLibraryError::RetainedSourceMismatch);
        }
    }

    let document_binding = catalogue
        .type_bindings()
        .get(31)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let bytestream_binding = catalogue
        .type_bindings()
        .get(32)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let output_bindings = [document_binding, bytestream_binding];
    for (export, (definition, binding)) in parsed
        .type_exports()
        .iter()
        .zip(output_definitions.iter().zip(output_bindings))
    {
        if !matches_qualified_export(export, definition.name(), definition.id(), binding) {
            return Err(StandardLibraryError::RetainedSourceMismatch);
        }
    }

    let origin = |span: &orna_syntax::SourceSpan| -> Result<SourceOrigin, StandardLibraryError> {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end =
            u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        SourceOrigin::new(STD_OUTPUT_SOURCE_UNIT_ID, start, end)
            .map_err(|source| StandardLibraryError::Revision { source })
    };

    let expected_identities = [
        DefinitionIdentity::Schema(STD_TERMINAL_SCHEMA_ID),
        DefinitionIdentity::Schema(STD_IO_SCHEMA_ID),
        DefinitionIdentity::ValueType(STD_TERMINAL_DOCUMENT_TYPE_ID),
        DefinitionIdentity::TypeBinding(document_binding.id()),
        DefinitionIdentity::ValueType(STD_IO_BYTE_STREAM_TYPE_ID),
        DefinitionIdentity::TypeBinding(bytestream_binding.id()),
    ];
    let mut declarations = vec![
        (
            parsed.schemas()[0].span.clone(),
            DefinitionIdentity::Schema(STD_TERMINAL_SCHEMA_ID),
        ),
        (
            parsed.schemas()[1].span.clone(),
            DefinitionIdentity::Schema(STD_IO_SCHEMA_ID),
        ),
        (
            parsed.opaque_value_types()[0].span.clone(),
            DefinitionIdentity::ValueType(STD_TERMINAL_DOCUMENT_TYPE_ID),
        ),
        (
            parsed.type_exports()[0].span.clone(),
            DefinitionIdentity::TypeBinding(document_binding.id()),
        ),
        (
            parsed.opaque_value_types()[1].span.clone(),
            DefinitionIdentity::ValueType(STD_IO_BYTE_STREAM_TYPE_ID),
        ),
        (
            parsed.type_exports()[1].span.clone(),
            DefinitionIdentity::TypeBinding(bytestream_binding.id()),
        ),
    ];
    declarations.sort_by_key(|(span, _)| span.start);
    if declarations
        .iter()
        .map(|(_, identity)| *identity)
        .ne(expected_identities.iter().copied())
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    declarations
        .into_iter()
        .map(|(span, identity)| Ok(DefinitionOrigin::new(identity, origin(&span)?)))
        .collect()
}

pub(super) fn retained_standard_library_v2_snapshot_from_source(
    types_source: &str,
    invoke_source: &str,
) -> Result<StandardLibrarySnapshot, StandardLibraryError> {
    let manifest = standard_library_v2_manifest()
        .map_err(|source| StandardLibraryError::Manifest { source })?;
    let types_manifest =
        standard_library_manifest().map_err(|source| StandardLibraryError::Manifest { source })?;
    let catalogue = manifest.catalogue();

    let mut origins = reconcile_retained_source_with_unit(
        types_source,
        &types_manifest,
        STD_TYPES_SOURCE_UNIT_ID,
    )?;
    let invoke_origins = reconcile_retained_invoke_source(invoke_source, catalogue)?;
    origins.extend(invoke_origins.iter().cloned());

    let types_content_hash = source_unit_content_digest(types_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if types_content_hash != ACCEPTED_V2_TYPES_CONTENT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let invoke_content_hash = source_unit_content_digest(invoke_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if invoke_content_hash != ACCEPTED_V2_INVOKE_CONTENT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let types_unit = StoredSourceUnit::new(
        STD_TYPES_SOURCE_UNIT_ID,
        0,
        SOURCE_LOGICAL_PATH,
        types_source,
        types_content_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let invoke_unit = StoredSourceUnit::new(
        STD_INVOKE_SOURCE_UNIT_ID,
        1,
        STD_INVOKE_SOURCE_LOGICAL_PATH,
        invoke_source,
        invoke_content_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let units = vec![types_unit, invoke_unit];
    let bundle_hash = source_bundle_digest(&units)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if bundle_hash != ACCEPTED_V2_SOURCE_BUNDLE_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let revision_hash = source_revision_record_digest(
        STANDARD_SOURCE_V2_BUNDLE_ID,
        Some(STANDARD_SOURCE_REVISION_ID),
        bundle_hash,
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if revision_hash != ACCEPTED_V2_SOURCE_REVISION_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let retained_source = StoredSourceRevision::new(
        STANDARD_SOURCE_V2_BUNDLE_ID,
        STANDARD_SOURCE_V2_REVISION_ID,
        Some(STANDARD_SOURCE_REVISION_ID),
        units,
        bundle_hash,
        revision_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;

    let executable = retained_v2_executable(invoke_source, catalogue, &invoke_origins)?;
    let snapshot = StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V2_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        retained_source,
        LANGUAGE_VERSION_IDENTITY,
        catalogue.clone(),
        vec![executable],
        origins,
        ACCEPTED_V2_STANDARD_LIBRARY_DIGEST,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let _ = standard_library_digest(&snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;

    Ok(snapshot)
}
pub(super) fn retained_standard_library_snapshot_from_source(
    source: &str,
) -> Result<StandardLibrarySnapshot, StandardLibraryError> {
    let manifest =
        standard_library_manifest().map_err(|source| StandardLibraryError::Manifest { source })?;
    let origins = reconcile_retained_source(source, &manifest)?;

    let content_hash = source_unit_content_digest(source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if content_hash != ACCEPTED_SOURCE_CONTENT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let unit = StoredSourceUnit::new(
        STANDARD_SOURCE_UNIT_ID,
        0,
        SOURCE_LOGICAL_PATH,
        source,
        content_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let units = vec![unit];
    let bundle_hash = source_bundle_digest(&units)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if bundle_hash != ACCEPTED_SOURCE_BUNDLE_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let revision_hash = source_revision_record_digest(STANDARD_SOURCE_BUNDLE_ID, None, bundle_hash)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if revision_hash != ACCEPTED_SOURCE_REVISION_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let retained_source = StoredSourceRevision::new(
        STANDARD_SOURCE_BUNDLE_ID,
        STANDARD_SOURCE_REVISION_ID,
        None,
        units,
        bundle_hash,
        revision_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let snapshot = StandardLibrarySnapshot::new(
        STANDARD_LIBRARY_REVISION_ID,
        StandardLibraryDigestVersion::Version1,
        retained_source,
        LANGUAGE_VERSION_IDENTITY,
        manifest.catalogue().clone(),
        origins,
        ACCEPTED_STANDARD_LIBRARY_DIGEST,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let _ = standard_library_digest(&snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;

    Ok(snapshot)
}

fn reconcile_retained_source(
    source: &str,
    manifest: &StandardLibraryManifest,
) -> Result<Vec<DefinitionOrigin>, StandardLibraryError> {
    reconcile_retained_source_with_unit(source, manifest, STANDARD_SOURCE_UNIT_ID)
}

/// Reconciles one retained `std/types.orna` source unit against the
/// source-independent type manifest.
///
/// The V1 snapshot retains the type declarations with the `orna.std/1`
/// source-unit identity `...01`. The V2 snapshot retains the exact same bytes
/// with the new durable unit identity `...02`; the declarations and their
/// byte ranges are identical, so this function differs only in the unit
/// identity attached to every origin.
pub(super) fn reconcile_retained_source_with_unit(
    source: &str,
    manifest: &StandardLibraryManifest,
    unit_id: SourceUnitId,
) -> Result<Vec<DefinitionOrigin>, StandardLibraryError> {
    let parsed = orna_syntax::parse(source);
    if !parsed.diagnostics().is_empty()
        || parsed.syntax().text() != source
        || !parsed.object_types().is_empty()
        || !parsed.field_renames().is_empty()
        || !parsed.server_functions().is_empty()
        || !parsed.client_functions().is_empty()
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    let catalogue = manifest.catalogue();
    if parsed.schemas().len() != catalogue.schemas().len()
        || parsed.primitive_value_types().len() + parsed.opaque_value_types().len()
            != catalogue.value_types().len()
        || parsed.type_exports().len() != catalogue.type_bindings().len()
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    let mut declarations = Vec::with_capacity(45);
    for (declaration, schema) in parsed.schemas().iter().zip(catalogue.schemas()) {
        if !matches_qualified_name(&declaration.name, schema.name()) {
            return Err(StandardLibraryError::RetainedSourceMismatch);
        }
        declarations.push((
            declaration.span.clone(),
            DefinitionIdentity::Schema(schema.id()),
        ));
    }

    let mut binding_index = 0;
    for (declaration, definition) in parsed
        .primitive_value_types()
        .iter()
        .zip(catalogue.value_types())
    {
        if definition.kind() != ValueTypeKind::Primitive
            || !matches_qualified_name(&declaration.name, definition.name())
            || decode_sql_string_literal(&declaration.kernel_contract.text).as_deref()
                != Some(definition.representation_contract())
            || source_persistence(declaration.persistence) != definition.persistence()
        {
            return Err(StandardLibraryError::RetainedSourceMismatch);
        }
        declarations.push((
            declaration.span.clone(),
            DefinitionIdentity::ValueType(definition.id()),
        ));

        let qualified = catalogue
            .type_bindings()
            .get(binding_index)
            .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
        let export = parsed
            .type_exports()
            .get(binding_index)
            .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
        if !matches_qualified_export(export, definition.name(), definition.id(), qualified) {
            return Err(StandardLibraryError::RetainedSourceMismatch);
        }
        declarations.push((
            export.span.clone(),
            DefinitionIdentity::TypeBinding(qualified.id()),
        ));
        binding_index += 1;

        while let Some(prelude) = catalogue.type_bindings().get(binding_index) {
            if prelude.kind() != orna_core::catalogue::TypeBindingKind::Prelude
                || prelude.target() != definition.id()
            {
                break;
            }
            let export = parsed
                .type_exports()
                .get(binding_index)
                .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
            if !matches_prelude_export(export, qualified, prelude) {
                return Err(StandardLibraryError::RetainedSourceMismatch);
            }
            declarations.push((
                export.span.clone(),
                DefinitionIdentity::TypeBinding(prelude.id()),
            ));
            binding_index += 1;
        }
    }

    let declaration = parsed
        .opaque_value_types()
        .first()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let definition = catalogue
        .value_types()
        .get(VALUE_TYPE_FACTS.len())
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    if parsed.opaque_value_types().len() != 1
        || definition.kind() != ValueTypeKind::Opaque
        || !matches_qualified_name(&declaration.name, definition.name())
        || decode_sql_string_literal(&declaration.kernel_contract.text).as_deref()
            != Some(definition.representation_contract())
        || definition.persistence() != ValueTypePersistence::Transient
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    declarations.push((
        declaration.span.clone(),
        DefinitionIdentity::ValueType(definition.id()),
    ));
    let qualified = catalogue
        .type_bindings()
        .get(binding_index)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let export = parsed
        .type_exports()
        .get(binding_index)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    if !matches_qualified_export(export, definition.name(), definition.id(), qualified) {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    declarations.push((
        export.span.clone(),
        DefinitionIdentity::TypeBinding(qualified.id()),
    ));
    binding_index += 1;

    if binding_index != catalogue.type_bindings().len() || declarations.len() != 47 {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    let expected_identities = declarations
        .iter()
        .map(|(_, identity)| *identity)
        .collect::<Vec<_>>();
    declarations.sort_by_key(|(span, _)| span.start);
    if declarations
        .iter()
        .map(|(_, identity)| *identity)
        .ne(expected_identities)
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    declarations
        .into_iter()
        .map(|(span, identity)| {
            let start = u32::try_from(span.start)
                .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
            let end = u32::try_from(span.end)
                .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
            let source = SourceOrigin::new(unit_id, start, end)
                .map_err(|source| StandardLibraryError::Revision { source })?;
            Ok(DefinitionOrigin::new(identity, source))
        })
        .collect()
}

/// Reconciles the retained `std/invoke.orna` unit against the V2 catalogue.
///
/// The unit must round-trip exactly and contain nothing besides the
/// `CREATE SCHEMA std.invoke;` declaration and the one `std.invoke.echo`
/// server function. The complete origin set is exactly the `std.invoke`
/// schema declaration, the function declaration, and the `p_value` parameter
/// declaration, each at its exact byte range in the retained unit. The closed
/// executable shape (parameter, result, security, transaction, volatility,
/// body, artifact, and references) is checked by the canonical compiler
/// checker in [`retained_v2_executable`].
pub(super) fn reconcile_retained_invoke_source(
    source: &str,
    catalogue: &CatalogueSnapshot,
) -> Result<Vec<DefinitionOrigin>, StandardLibraryError> {
    let parsed = orna_syntax::parse(source);
    if !parsed.diagnostics().is_empty()
        || parsed.syntax().text() != source
        || !parsed.object_types().is_empty()
        || !parsed.field_renames().is_empty()
        || !parsed.primitive_value_types().is_empty()
        || !parsed.opaque_value_types().is_empty()
        || !parsed.record_value_types().is_empty()
        || !parsed.enum_types().is_empty()
        || !parsed.type_exports().is_empty()
        || !parsed.client_functions().is_empty()
        || parsed.schemas().len() != 1
        || parsed.server_functions().len() != 1
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }

    let schema = &parsed.schemas()[0];
    let function = &parsed.server_functions()[0];
    let expected_schema = catalogue
        .schema_by_id(STD_INVOKE_SCHEMA_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    let expected_function = catalogue
        .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;
    if !matches_qualified_name(&schema.name, expected_schema.name())
        || !matches_qualified_name(&function.name, expected_function.name())
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let parameter = function
        .parameters
        .first()
        .ok_or(StandardLibraryError::RetainedSourceMismatch)?;

    let origin = |span: &orna_syntax::SourceSpan| -> Result<SourceOrigin, StandardLibraryError> {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end =
            u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        SourceOrigin::new(STD_INVOKE_SOURCE_UNIT_ID, start, end)
            .map_err(|source| StandardLibraryError::Revision { source })
    };

    Ok(vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(STD_INVOKE_SCHEMA_ID),
            origin(&schema.span)?,
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Function(STD_INVOKE_ECHO_FUNCTION_ID),
            origin(&function.span)?,
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Parameter {
                owner: STD_INVOKE_ECHO_FUNCTION_ID,
                parameter: STD_INVOKE_ECHO_PARAMETER_ID,
            },
            origin(&parameter.span)?,
        ),
    ])
}
