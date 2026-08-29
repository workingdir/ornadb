use super::*;

mod presenters;

use presenters::standard_ui_constructor_spec;
pub use presenters::{
    check_standard_json_encode, check_standard_terminal_present_table,
    check_standard_ui_constructor, check_standard_ui_window,
};

/// Checks retained standard source against its verified catalogue and origins.
///
/// Version 1 keeps the original one-unit, type-only reconcile contract. The
/// V2 contract (`StandardLibraryDigestVersion::Version2`) additionally
/// reconciles the ordered two-unit bundle (`std/types.orna` then
/// `std/invoke.orna`), the fixed identities, the exact `std.invoke.echo`
/// executable (artifact, semantic digest, and three durable references), and
/// every schema, function, and parameter origin against the retained units.
/// The V3 standard revision (ADR 0058) reuses the V2 digest contract but
/// carries the ordered three-unit bundle (`std/types.orna`,
/// `std/invoke.orna`, then `std/output.orna`); its branch reconciles the first
/// two units exactly as V2 does and additionally reconciles the output unit
/// closed against the `std.terminal` and `std.io` schemas, the two opaque
/// output value types, their exports, and every origin on the retained unit.
/// The V4 standard revision (ADR 0062) carries the ordered four-unit bundle
/// (`std/types.orna`, `std/invoke.orna`, `std/output.orna`, then `std/ui.orna`);
/// its branch reconciles the first
/// three units exactly as V3 does and additionally reconciles the ui unit,
/// opaque `std.ui.ui` value type, its export, and every origin on the retained
/// unit. The V5 standard revision (ADR 0075) retains those four units and adds
/// `std/json.orna`; its explicit branch reconciles the JSON schema, opaque value
/// type, export, and existing `std.json.encode` presenter against the installed
/// catalogue. The checker does not trust a source file because its path looks
/// standard.
pub fn check_standard_library_source(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    match snapshot.digest_version() {
        StandardLibraryDigestVersion::Version1 => check_standard_library_source_v1(snapshot),
        StandardLibraryDigestVersion::Version2 => match snapshot.revision() {
            STANDARD_LIBRARY_V10_REVISION_ID => check_standard_library_source_v10(snapshot),
            STANDARD_LIBRARY_V9_REVISION_ID => check_standard_library_source_v9(snapshot),
            STANDARD_LIBRARY_V8_REVISION_ID => check_standard_library_source_v8(snapshot),
            STANDARD_LIBRARY_V7_REVISION_ID => check_standard_library_source_v7(snapshot),
            STANDARD_LIBRARY_V6_REVISION_ID => check_standard_library_source_v6(snapshot),
            STANDARD_LIBRARY_V5_REVISION_ID => check_standard_library_source_v5(snapshot),
            STANDARD_LIBRARY_V4_REVISION_ID => check_standard_library_source_v4(snapshot),
            STANDARD_LIBRARY_V3_REVISION_ID => check_standard_library_source_v3(snapshot),
            _ => check_standard_library_source_v2(snapshot),
        },
        _ => Err(StandardLibraryCheckError::SourceMismatch),
    }
}

const STANDARD_SOURCE_UNIT_ID: SourceUnitId =
    SourceUnitId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);

pub(super) fn check_standard_library_source_v1_identity(
    stored_unit: &StoredSourceUnit,
) -> Result<(), StandardLibraryCheckError> {
    if stored_unit.id() != STANDARD_SOURCE_UNIT_ID
        || stored_unit.logical_path() != "std/types.orna"
        || stored_unit.ordinal() != 0
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    Ok(())
}

/// Checks one retained version-1 type-only standard source unit.
///
/// This is the original `orna.std/1` contract: exactly one source unit, no
/// functions, and the full schema/value-type/binding reconcile.
fn check_standard_library_source_v1(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    let source_units = snapshot.source().units();
    let [stored_unit] = source_units else {
        return Err(StandardLibraryCheckError::SourceUnitCount {
            actual: source_units.len(),
        });
    };
    check_standard_library_source_v1_identity(stored_unit)?;

    let bundle = SourceBundle::new([SourceUnit::new(
        stored_unit.logical_path(),
        stored_unit.content(),
    )])
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let report = parse_bundle(&bundle);
    if !report.diagnostics().is_empty() {
        return Err(StandardLibraryCheckError::Diagnostics {
            diagnostics: report.diagnostics().to_vec(),
        });
    }
    let parsed_unit = report
        .units()
        .first()
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let families = reconcile_standard_source(
        stored_unit,
        parsed_unit,
        snapshot.catalogue(),
        snapshot.origins(),
    )?;

    Ok(CheckedStandardLibrary {
        verified_snapshot: snapshot.clone(),
        schemas: families.schemas,
        value_types: families.value_types,
        type_bindings: families.type_bindings,
        checked_executables: Vec::new(),
    })
}

/// Checks one retained V2 executable standard source bundle.
///
/// The ordered bundle must be exactly `std/types.orna` (`...02`) followed by
/// `std/invoke.orna` (`...03`). The types unit reconciles exactly as V1 does.
/// The invoke unit must contain exactly the `std.invoke` schema declaration
/// and the `std.invoke.echo` server function; the function is checked closed
/// by [`check_standard_parameter_echo`], and every stored executable fact
/// (function and revision identities, revision number, semantic-hash
/// contract, declaration origin and content hash, semantic digest, language
/// version, artifact, and the three ordered references) plus every origin
/// must agree with the checked source facts or the snapshot fails closed.
fn check_standard_library_source_v2(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    let (families, checked_executable) = check_standard_library_source_v2_parts(
        snapshot.source(),
        snapshot.catalogue(),
        snapshot.origins(),
        snapshot.executables(),
    )?;
    Ok(CheckedStandardLibrary {
        verified_snapshot: snapshot.clone(),
        schemas: families.schemas,
        value_types: families.value_types,
        type_bindings: families.type_bindings,
        checked_executables: vec![checked_executable],
    })
}

/// Checks the retained V2 source bundle, catalogue, origins, and executable
/// evidence without a retained digest.
///
/// The digest gate is a separate, prior verification step
/// (`verify_standard_library_v2_snapshot`); this function reconciles the
/// source facts against the supplied stored facts and fails closed on any
/// disagreement. The checked executable facts fix the ADR 0055 language
/// version `orna.language/1`; a snapshot that retained any other label fails
/// the stored-executable cross-check.
pub(super) fn check_standard_library_source_v2_parts(
    source: &StoredSourceRevision,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    let source_units = source.units();
    let [types_unit, invoke_unit] = source_units else {
        return Err(StandardLibraryCheckError::SourceUnitCount {
            actual: source_units.len(),
        });
    };
    if types_unit.id() != STD_TYPES_SOURCE_UNIT_ID
        || types_unit.logical_path() != "std/types.orna"
        || types_unit.ordinal() != 0
        || invoke_unit.id() != STD_INVOKE_SOURCE_UNIT_ID
        || invoke_unit.logical_path() != "std/invoke.orna"
        || invoke_unit.ordinal() != 1
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let bundle = SourceBundle::new([
        SourceUnit::new(types_unit.logical_path(), types_unit.content()),
        SourceUnit::new(invoke_unit.logical_path(), invoke_unit.content()),
    ])
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let report = parse_bundle(&bundle);
    if !report.diagnostics().is_empty() {
        return Err(StandardLibraryCheckError::Diagnostics {
            diagnostics: report.diagnostics().to_vec(),
        });
    }
    let [parsed_types, parsed_invoke] = report.units() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };

    let (types_origins, invoke_origins) = partition_standard_origins(origins)?;
    let types_catalogue = standard_types_catalogue(catalogue)?;
    let families =
        reconcile_standard_source(types_unit, parsed_types, &types_catalogue, &types_origins)?;
    let checked_executable = reconcile_standard_invoke_executable(
        catalogue,
        &invoke_origins,
        executables,
        invoke_unit,
        parsed_invoke,
    )?;
    Ok((families, checked_executable))
}

/// Checks one retained V3 output standard source bundle.
///
/// The ordered bundle must be exactly `std/types.orna` (`...02`),
/// `std/invoke.orna` (`...03`), then `std/output.orna` (`...04`). Units zero
/// and one reconcile exactly as the V2 checker does, including the unchanged
/// `std.invoke.echo` executable. The output unit must declare exactly the
/// `std.terminal` (`...04`) and `std.io` (`...05`) schemas, the two opaque
/// value types `std.terminal.Document` (`...15`) and `std.io.ByteStream`
/// (`...16`) with their ADR 0058 kernel contracts and `IMMUTABLE TRANSIENT`
/// catalogue facts, and the two qualified exports (`std.Document`,
/// `std.ByteStream`); every origin must sit on the retained output unit at
/// the exact declaration byte ranges, and any extra, missing, or mismatched
/// declaration, identity, contract, binding, or origin fails closed.
fn check_standard_library_source_v3(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    let (families, checked_executable) = check_standard_library_source_v3_parts(
        snapshot.source(),
        snapshot.catalogue(),
        snapshot.origins(),
        snapshot.executables(),
    )?;
    Ok(CheckedStandardLibrary {
        verified_snapshot: snapshot.clone(),
        schemas: families.schemas,
        value_types: families.value_types,
        type_bindings: families.type_bindings,
        checked_executables: vec![checked_executable],
    })
}

/// Checks the retained V3 source bundle, catalogue, origins, and executable
/// evidence without a retained digest.
///
/// The digest gate is a separate, prior verification step
/// (`verify_standard_library_v3_snapshot`); this function reconciles the
/// source facts against the supplied stored facts and fails closed on any
/// disagreement, exactly as [`check_standard_library_source_v2_parts`] does
/// for the first two units, then reconciles the output unit.
pub(super) fn check_standard_library_source_v3_parts(
    source: &StoredSourceRevision,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    let source_units = source.units();
    let [types_unit, invoke_unit, output_unit] = source_units else {
        return Err(StandardLibraryCheckError::SourceUnitCount {
            actual: source_units.len(),
        });
    };
    if types_unit.id() != STD_TYPES_SOURCE_UNIT_ID
        || types_unit.logical_path() != "std/types.orna"
        || types_unit.ordinal() != 0
        || invoke_unit.id() != STD_INVOKE_SOURCE_UNIT_ID
        || invoke_unit.logical_path() != "std/invoke.orna"
        || invoke_unit.ordinal() != 1
        || output_unit.id() != STD_OUTPUT_SOURCE_UNIT_ID
        || output_unit.logical_path() != "std/output.orna"
        || output_unit.ordinal() != 2
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let bundle = SourceBundle::new([
        SourceUnit::new(types_unit.logical_path(), types_unit.content()),
        SourceUnit::new(invoke_unit.logical_path(), invoke_unit.content()),
        SourceUnit::new(output_unit.logical_path(), output_unit.content()),
    ])
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let report = parse_bundle(&bundle);
    if !report.diagnostics().is_empty() {
        return Err(StandardLibraryCheckError::Diagnostics {
            diagnostics: report.diagnostics().to_vec(),
        });
    }
    let [parsed_types, parsed_invoke, parsed_output] = report.units() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };

    let (types_origins, invoke_origins, output_origins) = partition_standard_v3_origins(origins)?;
    let types_catalogue = standard_v3_types_catalogue(catalogue)?;
    let families =
        reconcile_standard_source(types_unit, parsed_types, &types_catalogue, &types_origins)?;
    let checked_executable = reconcile_standard_invoke_executable(
        catalogue,
        &invoke_origins,
        executables,
        invoke_unit,
        parsed_invoke,
    )?;
    reconcile_standard_output_unit(output_unit, parsed_output, catalogue, &output_origins)?;
    Ok((families, checked_executable))
}

/// Checks one retained V4 UI standard source bundle.
///
/// The ordered bundle must be exactly `std/types.orna` (`...02`),
/// `std/invoke.orna` (`...03`), `std/output.orna` (`...04`), then
/// `std/ui.orna` (`...05`). Units zero to two reconcile exactly as the V3
/// checker does, including the unchanged `std.invoke.echo` executable. The
/// ui unit must declare exactly the `std.ui` (`...08`) schema, the single
/// opaque value type `std.ui.UI` (`...19`) with its ADR 0062 kernel contract
/// and `IMMUTABLE TRANSIENT` catalogue facts, and the single qualified
/// export (`std.UI`); every origin must sit on the retained ui unit at the
/// exact declaration byte ranges, and any extra, missing, or mismatched
/// declaration, identity, contract, binding, or origin fails closed.
fn check_standard_library_source_v4(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    let (families, checked_executable) = check_standard_library_source_v4_parts(
        snapshot.source(),
        snapshot.catalogue(),
        snapshot.origins(),
        snapshot.executables(),
    )?;
    Ok(CheckedStandardLibrary {
        verified_snapshot: snapshot.clone(),
        schemas: families.schemas,
        value_types: families.value_types,
        type_bindings: families.type_bindings,
        checked_executables: vec![checked_executable],
    })
}

/// Checks the retained V4 source bundle, catalogue, origins, and executable
/// evidence without a retained digest.
///
/// The digest gate is a separate, prior verification step
/// (`verify_standard_library_v4_snapshot`); this function reconciles the
/// source facts against the supplied stored facts and fails closed on any
/// disagreement, exactly as [`check_standard_library_source_v3_parts`] does
/// for the first three units, then reconciles the ui unit.
pub(super) fn check_standard_library_source_v4_parts(
    source: &StoredSourceRevision,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    let types_catalogue = standard_v4_types_catalogue(catalogue)?;
    let origin_partitions = partition_standard_v4_origins(origins)?;
    check_standard_library_source_v4_units(
        source.units(),
        catalogue,
        executables,
        &types_catalogue,
        origin_partitions,
    )
}

fn check_standard_library_source_v4_units(
    source_units: &[StoredSourceUnit],
    catalogue: &CatalogueSnapshot,
    executables: &[StandardExecutable],
    types_catalogue: &CatalogueSnapshot,
    (types_origins, invoke_origins, output_origins, ui_origins): StandardV4OriginPartitions,
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    let [types_unit, invoke_unit, output_unit, ui_unit] = source_units else {
        return Err(StandardLibraryCheckError::SourceUnitCount {
            actual: source_units.len(),
        });
    };
    if types_unit.id() != STD_TYPES_SOURCE_UNIT_ID
        || types_unit.logical_path() != "std/types.orna"
        || types_unit.ordinal() != 0
        || invoke_unit.id() != STD_INVOKE_SOURCE_UNIT_ID
        || invoke_unit.logical_path() != "std/invoke.orna"
        || invoke_unit.ordinal() != 1
        || output_unit.id() != STD_OUTPUT_SOURCE_UNIT_ID
        || output_unit.logical_path() != "std/output.orna"
        || output_unit.ordinal() != 2
        || ui_unit.id() != STD_UI_SOURCE_UNIT_ID
        || ui_unit.logical_path() != "std/ui.orna"
        || ui_unit.ordinal() != 3
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let bundle = SourceBundle::new([
        SourceUnit::new(types_unit.logical_path(), types_unit.content()),
        SourceUnit::new(invoke_unit.logical_path(), invoke_unit.content()),
        SourceUnit::new(output_unit.logical_path(), output_unit.content()),
        SourceUnit::new(ui_unit.logical_path(), ui_unit.content()),
    ])
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let report = parse_bundle(&bundle);
    if !report.diagnostics().is_empty() {
        return Err(StandardLibraryCheckError::Diagnostics {
            diagnostics: report.diagnostics().to_vec(),
        });
    }
    let [parsed_types, parsed_invoke, parsed_output, parsed_ui] = report.units() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };

    let families =
        reconcile_standard_source(types_unit, parsed_types, types_catalogue, &types_origins)?;
    let checked_executable = reconcile_standard_invoke_executable(
        catalogue,
        &invoke_origins,
        executables,
        invoke_unit,
        parsed_invoke,
    )?;
    reconcile_standard_output_unit(output_unit, parsed_output, catalogue, &output_origins)?;
    reconcile_standard_ui_unit(ui_unit, parsed_ui, catalogue, &ui_origins)?;
    Ok((families, checked_executable))
}

/// Scopes the V2 catalogue to the declarations retained in `std/types.orna`:
/// the standard schemas, value types, and type bindings only.
///
/// The `std.invoke` schema and the standard functions are declared in
/// `std/invoke.orna` and are reconciled by the invoke path; the V1 type-only
/// reconcile contract must not see them. Any other catalogue schema, function,
/// object, enum, or record type fails closed.
fn standard_types_catalogue(
    catalogue: &CatalogueSnapshot,
) -> Result<CatalogueSnapshot, StandardLibraryCheckError> {
    scope_standard_catalogue(catalogue, &[STD_INVOKE_SCHEMA_ID], &[])
}
fn check_standard_source_units(
    source_units: &[StoredSourceUnit],
    expected: &[(SourceUnitId, &str, u32)],
) -> Result<(), StandardLibraryCheckError> {
    if source_units.len() != expected.len()
        || source_units
            .iter()
            .zip(expected)
            .any(|(unit, (id, path, ordinal))| {
                unit.id() != *id || unit.logical_path() != *path || unit.ordinal() != *ordinal
            })
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    Ok(())
}

fn checked_standard_json_executable_for_snapshot(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardExecutable, StandardLibraryCheckError> {
    let json_unit = snapshot
        .source()
        .units()
        .iter()
        .find(|unit| unit.id() == STD_JSON_SOURCE_UNIT_ID)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let json_origins = snapshot
        .origins()
        .iter()
        .filter(|origin| origin.source().source_unit() == STD_JSON_SOURCE_UNIT_ID)
        .cloned()
        .collect::<Vec<_>>();
    let json_bundle = SourceBundle::new([SourceUnit::new(
        json_unit.logical_path(),
        json_unit.content(),
    )])
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let report = parse_bundle(&json_bundle);
    if !report.diagnostics().is_empty() {
        return Err(StandardLibraryCheckError::Diagnostics {
            diagnostics: report.diagnostics().to_vec(),
        });
    }
    let [parsed] = report.units() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [declaration] = parsed.parsed().server_functions() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let record = expected_standard_json_executable(
        declaration,
        snapshot.catalogue(),
        &json_origins,
        json_unit,
    )?;
    let schema_origin = json_origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Schema(STD_JSON_SCHEMA_ID))
        .map(DefinitionOrigin::source)
        .ok_or(StandardLibraryCheckError::MissingSchemaOrigin)?;
    checked_standard_executable_from_record(
        &record,
        snapshot.catalogue(),
        &json_origins,
        schema_origin,
    )
}

/// Checks one retained V5 JSON standard source bundle.
fn check_standard_library_source_v5(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    let (families, checked_executable) = check_standard_library_source_v5_parts(
        snapshot.source().units(),
        snapshot.catalogue(),
        snapshot.origins(),
        snapshot.executables(),
    )?;
    let checked_json = checked_standard_json_executable_for_snapshot(snapshot)?;
    Ok(CheckedStandardLibrary {
        verified_snapshot: snapshot.clone(),
        schemas: families.schemas,
        value_types: families.value_types,
        type_bindings: families.type_bindings,
        checked_executables: vec![checked_executable, checked_json],
    })
}

/// Checks one retained V6 action standard source bundle.
fn check_standard_library_source_v6(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    let (families, checked_executable) = check_standard_library_source_v6_parts(
        snapshot.source().units(),
        snapshot.catalogue(),
        snapshot.origins(),
        snapshot.executables(),
    )?;
    let checked_json = checked_standard_json_executable_for_snapshot(snapshot)?;
    Ok(CheckedStandardLibrary {
        verified_snapshot: snapshot.clone(),
        schemas: families.schemas,
        value_types: families.value_types,
        type_bindings: families.type_bindings,
        checked_executables: vec![checked_executable, checked_json],
    })
}

/// Checks one retained V7 standard source bundle.
fn check_standard_library_source_v7(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    let (families, checked_executables) = check_standard_library_source_v7_parts(
        snapshot.source().units(),
        snapshot.catalogue(),
        snapshot.origins(),
        snapshot.executables(),
    )?;
    Ok(CheckedStandardLibrary {
        verified_snapshot: snapshot.clone(),
        schemas: families.schemas,
        value_types: families.value_types,
        type_bindings: families.type_bindings,
        checked_executables,
    })
}

fn check_standard_library_source_v7_parts(
    source_units: &[StoredSourceUnit],
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, Vec<CheckedStandardExecutable>), StandardLibraryCheckError> {
    let [
        types_unit,
        invoke_unit,
        output_unit,
        ui_unit,
        json_unit,
        action_unit,
        window_unit,
    ] = source_units
    else {
        return Err(StandardLibraryCheckError::SourceUnitCount {
            actual: source_units.len(),
        });
    };
    check_standard_source_units(
        source_units,
        &[
            (STD_TYPES_SOURCE_UNIT_ID, "std/types.orna", 0),
            (STD_INVOKE_SOURCE_UNIT_ID, "std/invoke.orna", 1),
            (STD_OUTPUT_SOURCE_UNIT_ID, "std/output.orna", 2),
            (STD_UI_SOURCE_UNIT_ID, "std/ui.orna", 3),
            (STD_JSON_SOURCE_UNIT_ID, "std/json.orna", 4),
            (STD_ACTION_SOURCE_UNIT_ID, "std/action.orna", 5),
            (STD_WINDOW_SOURCE_UNIT_ID, "std/window.orna", 6),
        ],
    )?;
    if executables.len() != 3 {
        return Err(StandardLibraryCheckError::ExecutableCount {
            actual: executables.len(),
        });
    }
    let echo_executable = executables
        .iter()
        .find(|executable| executable.function() == STD_INVOKE_ECHO_FUNCTION_ID)
        .ok_or(StandardLibraryCheckError::ExecutableMismatch)?;
    let json_executable = executables
        .iter()
        .find(|executable| executable.function() == STD_JSON_ENCODE_FUNCTION_ID)
        .ok_or(StandardLibraryCheckError::ExecutableMismatch)?;
    let window_executable = executables
        .iter()
        .find(|executable| executable.function() == STD_UI_WINDOW_FUNCTION_ID)
        .ok_or(StandardLibraryCheckError::ExecutableMismatch)?;
    if [
        echo_executable.function(),
        json_executable.function(),
        window_executable.function(),
    ]
    .into_iter()
    .enumerate()
    .any(|(index, function)| {
        [
            STD_INVOKE_ECHO_FUNCTION_ID,
            STD_JSON_ENCODE_FUNCTION_ID,
            STD_UI_WINDOW_FUNCTION_ID,
        ]
        .into_iter()
        .position(|expected| expected == function)
            != Some(index)
    }) {
        return Err(StandardLibraryCheckError::ExecutableMismatch);
    }

    let mut v6_origins = Vec::with_capacity(origins.len());
    let mut window_origins = Vec::new();
    for origin in origins {
        if origin.source().source_unit() == STD_WINDOW_SOURCE_UNIT_ID {
            window_origins.push(origin.clone());
        } else {
            v6_origins.push(origin.clone());
        }
    }
    let v6_executables = vec![echo_executable.clone(), json_executable.clone()];
    let (families, checked_echo) = check_standard_library_source_v6_parts(
        &[
            types_unit.clone(),
            invoke_unit.clone(),
            output_unit.clone(),
            ui_unit.clone(),
            json_unit.clone(),
            action_unit.clone(),
        ],
        catalogue,
        &v6_origins,
        &v6_executables,
    )?;

    let json_bundle = SourceBundle::new([SourceUnit::new(
        json_unit.logical_path(),
        json_unit.content(),
    )])
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let json_report = parse_bundle(&json_bundle);
    if !json_report.diagnostics().is_empty() {
        return Err(StandardLibraryCheckError::Diagnostics {
            diagnostics: json_report.diagnostics().to_vec(),
        });
    }
    let [parsed_json] = json_report.units() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [json_function] = parsed_json.parsed().server_functions() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let json_origins = v6_origins
        .iter()
        .filter(|origin| origin.source().source_unit() == STD_JSON_SOURCE_UNIT_ID)
        .cloned()
        .collect::<Vec<_>>();
    let json_record =
        expected_standard_json_executable(json_function, catalogue, &json_origins, json_unit)?;
    let json_schema_origin = json_origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Schema(STD_JSON_SCHEMA_ID))
        .map(DefinitionOrigin::source)
        .ok_or(StandardLibraryCheckError::MissingSchemaOrigin)?;
    let checked_json = checked_standard_executable_from_record(
        &json_record,
        catalogue,
        &json_origins,
        json_schema_origin,
    )?;

    let window_bundle = SourceBundle::new([SourceUnit::new(
        window_unit.logical_path(),
        window_unit.content(),
    )])
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let window_report = parse_bundle(&window_bundle);
    if !window_report.diagnostics().is_empty() {
        return Err(StandardLibraryCheckError::Diagnostics {
            diagnostics: window_report.diagnostics().to_vec(),
        });
    }
    let [parsed_window] = window_report.units() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let ui_schema_origin = origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Schema(STD_UI_SCHEMA_ID))
        .map(DefinitionOrigin::source)
        .ok_or(StandardLibraryCheckError::MissingSchemaOrigin)?;
    let checked_window = reconcile_standard_window_executable(
        catalogue,
        &window_origins,
        window_executable,
        window_unit,
        parsed_window,
        ui_schema_origin,
    )?;
    Ok((families, vec![checked_echo, checked_json, checked_window]))
}
/// Checks one retained V8 standard source bundle.
///
/// V8 is the append-only V7 child: it retains the seven historical units and
/// executable records byte-for-byte, then appends `std/data.orna` and the
/// retained terminal-table executable. The appended unit owns the `std.data`
/// schema, Rows value/export, and the table declaration; its result type is a
/// checked cross-unit reference to the retained terminal Document type.
fn check_standard_library_source_v8(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    let (families, checked_executables) = check_standard_library_source_v8_parts(
        snapshot.source().units(),
        snapshot.catalogue(),
        snapshot.origins(),
        snapshot.executables(),
    )?;
    Ok(CheckedStandardLibrary {
        verified_snapshot: snapshot.clone(),
        schemas: families.schemas,
        value_types: families.value_types,
        type_bindings: families.type_bindings,
        checked_executables,
    })
}

fn check_standard_library_source_v8_parts(
    source_units: &[StoredSourceUnit],
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, Vec<CheckedStandardExecutable>), StandardLibraryCheckError> {
    let [
        types_unit,
        invoke_unit,
        output_unit,
        ui_unit,
        json_unit,
        action_unit,
        window_unit,
        data_unit,
    ] = source_units
    else {
        return Err(StandardLibraryCheckError::SourceUnitCount {
            actual: source_units.len(),
        });
    };
    check_standard_source_units(
        source_units,
        &[
            (STD_TYPES_SOURCE_UNIT_ID, "std/types.orna", 0),
            (STD_INVOKE_SOURCE_UNIT_ID, "std/invoke.orna", 1),
            (STD_OUTPUT_SOURCE_UNIT_ID, "std/output.orna", 2),
            (STD_UI_SOURCE_UNIT_ID, "std/ui.orna", 3),
            (STD_JSON_SOURCE_UNIT_ID, "std/json.orna", 4),
            (STD_ACTION_SOURCE_UNIT_ID, "std/action.orna", 5),
            (STD_WINDOW_SOURCE_UNIT_ID, "std/window.orna", 6),
            (STD_DATA_SOURCE_UNIT_ID, "std/data.orna", 7),
        ],
    )?;
    if executables.len() != 4 {
        return Err(StandardLibraryCheckError::ExecutableCount {
            actual: executables.len(),
        });
    }
    let expected_functions = [
        STD_INVOKE_ECHO_FUNCTION_ID,
        STD_JSON_ENCODE_FUNCTION_ID,
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        STD_UI_WINDOW_FUNCTION_ID,
    ];
    if executables
        .iter()
        .map(StandardExecutable::function)
        .ne(expected_functions)
    {
        return Err(StandardLibraryCheckError::ExecutableMismatch);
    }

    let mut v7_origins = Vec::with_capacity(origins.len());
    let mut data_origins = Vec::new();
    for origin in origins {
        if origin.source().source_unit() == STD_DATA_SOURCE_UNIT_ID {
            data_origins.push(origin.clone());
        } else {
            v7_origins.push(origin.clone());
        }
    }
    let v7_executables = vec![
        executables[0].clone(),
        executables[1].clone(),
        executables[3].clone(),
    ];
    let v7_catalogue = CatalogueSnapshot::new_with_functions_and_types(
        catalogue.revision(),
        catalogue
            .schemas()
            .iter()
            .filter(|schema| schema.id() != STD_DATA_SCHEMA_ID)
            .cloned()
            .collect(),
        catalogue.object_types().to_vec(),
        catalogue
            .value_types()
            .iter()
            .filter(|value_type| value_type.id() != STD_DATA_ROWS_TYPE_ID)
            .cloned()
            .collect(),
        catalogue
            .type_bindings()
            .iter()
            .filter(|binding| binding.target() != STD_DATA_ROWS_TYPE_ID)
            .cloned()
            .collect(),
        catalogue
            .functions()
            .iter()
            .filter(|function| function.id() != STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID)
            .cloned()
            .collect(),
    )
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let (mut families, mut checked_executables) = check_standard_library_source_v7_parts(
        &[
            types_unit.clone(),
            invoke_unit.clone(),
            output_unit.clone(),
            ui_unit.clone(),
            json_unit.clone(),
            action_unit.clone(),
            window_unit.clone(),
        ],
        &v7_catalogue,
        &v7_origins,
        &v7_executables,
    )?;
    let data_bundle = SourceBundle::new([SourceUnit::new(
        data_unit.logical_path(),
        data_unit.content(),
    )])
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let data_report = parse_bundle(&data_bundle);
    if !data_report.diagnostics().is_empty() {
        return Err(StandardLibraryCheckError::Diagnostics {
            diagnostics: data_report.diagnostics().to_vec(),
        });
    }
    let [parsed_data] = data_report.units() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let data_families =
        reconcile_standard_data_unit(data_unit, parsed_data, catalogue, &data_origins)?;
    families.schemas.extend(data_families.schemas);
    families.value_types.extend(data_families.value_types);
    families.type_bindings.extend(data_families.type_bindings);

    let terminal_schema_origin = v7_origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Schema(STD_TERMINAL_SCHEMA_ID))
        .map(DefinitionOrigin::source)
        .ok_or(StandardLibraryCheckError::MissingSchemaOrigin)?;
    let table_executable = reconcile_standard_terminal_executable(
        catalogue,
        &data_origins,
        &executables[2],
        data_unit,
        parsed_data,
        terminal_schema_origin,
    )?;
    checked_executables.insert(2, table_executable);
    Ok((families, checked_executables))
}

/// Checks one retained V9 standard source bundle.
///
/// V9 retains the complete verified V8 Rows snapshot and appends the exact
/// `std/ui_constructors.orna` unit. The appended unit contributes no schema,
/// value type, or binding; it contributes exactly seven external CLIENT
/// constructor functions and their executable evidence.
fn check_standard_library_source_v9(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    let (families, checked_executables) = check_standard_library_source_v9_parts(
        snapshot.source().units(),
        snapshot.catalogue(),
        snapshot.origins(),
        snapshot.executables(),
    )?;
    Ok(CheckedStandardLibrary {
        verified_snapshot: snapshot.clone(),
        schemas: families.schemas,
        value_types: families.value_types,
        type_bindings: families.type_bindings,
        checked_executables,
    })
}

/// Checks the source-authored CLI session function retained by V10.
pub fn check_standard_cli_repl(
    declaration: &ClientFunctionDeclaration,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    stored_unit: &StoredSourceUnit,
) -> Result<CheckedStandardExecutable, StandardLibraryCheckError> {
    let expected_schema =
        QualifiedSemanticName::new(["std", "cli"]).expect("the fixed CLI schema is valid");
    let expected_function = QualifiedSemanticName::new(["std", "cli", "repl"])
        .expect("the fixed CLI function is valid");
    let expected_ui =
        QualifiedSemanticName::new(["std", "ui", "ui"]).expect("the fixed UI type is valid");
    if stored_unit.id() != STD_CLI_SOURCE_UNIT_ID
        || stored_unit.logical_path() != "std/cli.orna"
        || declaration.external
        || declaration.runtime_contract.is_some()
        || !declaration.capabilities.is_empty()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let FunctionReturnType::Single(result_type) = &declaration.return_type else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if !matches!(
        result_type,
        TypeSpecification::Named(name)
            if unquoted_semantic_name(name)? == expected_ui
                && resolved_standard_type_id(result_type, catalogue) == Some(STD_UI_TYPE_ID)
    ) {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let Some(ClientExpression::Call {
        callee: evaluate_callee,
        arguments: evaluate_arguments,
        ..
    }) = declaration.body.as_expression()
    else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if semantic_name(evaluate_callee)
        != QualifiedSemanticName::new(["std", "cli", "evaluate"])
            .expect("the fixed CLI evaluate intrinsic is valid")
        || evaluate_arguments.len() != 1
        || evaluate_arguments[0].name.is_some()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let ClientExpression::Call {
        callee: input_callee,
        arguments: input_arguments,
        ..
    } = &evaluate_arguments[0].value
    else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if semantic_name(input_callee)
        != QualifiedSemanticName::new(["std", "cli", "input"])
            .expect("the fixed CLI input intrinsic is valid")
        || !input_arguments.is_empty()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let schema = catalogue
        .schema_by_id(STD_CLI_SCHEMA_ID)
        .ok_or(StandardLibraryCheckError::MissingSchema)?;
    if schema.name() != &expected_schema {
        return Err(StandardLibraryCheckError::SchemaNameMismatch {
            actual: schema.name().clone(),
        });
    }
    let function = catalogue
        .function_by_id(STD_CLI_REPL_FUNCTION_ID)
        .ok_or(StandardLibraryCheckError::MissingFunction)?;
    if function.name() != &expected_function
        || function.domain() != FunctionDomain::Client
        || function.security() != CatalogueFunctionSecurity::Invoker
        || function.transaction().is_some()
        || function.volatility() != CatalogueFunctionVolatility::Volatile
        || function.current_revision() != STD_CLI_REPL_FUNCTION_REVISION_ID
        || !function.parameters().is_empty()
        || function.return_type() != &FunctionReturn::Single(ResolvedType::value(STD_UI_TYPE_ID))
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    if origins.len() != 2 {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let source_origin = |span: &SourceSpan| -> Result<SourceOrigin, StandardLibraryCheckError> {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
        let end = u32::try_from(span.end).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
        SourceOrigin::new(stored_unit.id(), start, end)
            .map_err(|_| StandardLibraryCheckError::SourceMismatch)
    };
    let expected_schema_origin = source_origin(
        &orna_syntax::parse(stored_unit.content())
            .schemas()
            .first()
            .ok_or(StandardLibraryCheckError::SourceMismatch)?
            .span,
    )?;
    let expected_function_origin = source_origin(&declaration.span)?;
    let schema_origin = origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Schema(STD_CLI_SCHEMA_ID))
        .map(DefinitionOrigin::source)
        .ok_or(StandardLibraryCheckError::MissingSchemaOrigin)?;
    let function_origin = origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Function(STD_CLI_REPL_FUNCTION_ID))
        .map(DefinitionOrigin::source)
        .ok_or(StandardLibraryCheckError::MissingFunctionOrigin)?;
    if schema_origin != expected_schema_origin
        || function_origin != expected_function_origin
        || origins.iter().any(|origin| {
            !matches!(
                origin.identity(),
                DefinitionIdentity::Schema(STD_CLI_SCHEMA_ID)
                    | DefinitionIdentity::Function(STD_CLI_REPL_FUNCTION_ID)
            )
        })
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let result_origin = source_origin(result_type.span())?;
    let references = vec![DefinitionReference::new(
        STD_CLI_REPL_FUNCTION_ID,
        STD_CLI_REPL_FUNCTION_REVISION_ID,
        0,
        DefinitionReferenceTarget::ValueType(STD_UI_TYPE_ID),
        DefinitionReferenceKind::NamedType,
        result_origin,
    )];
    let plan = ExpressionClientPlan::new(ClientExpressionNode::Evaluate {
        expression: Box::new(ClientExpressionNode::Input),
    });
    let payload = plan
        .encode()
        .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let artifact_hash = artifact_payload_digest(&payload)
        .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        CLIENT_PLAN_FORMAT,
        plan.format_version(),
        payload,
        artifact_hash,
    )
    .map_err(|source| StandardLibraryCheckError::Revision { source })?;
    let declaration_bytes = &stored_unit.content().as_bytes()
        [function_origin.byte_start() as usize..function_origin.byte_end() as usize];
    let declaration_content_hash = function_declaration_digest(declaration_bytes)
        .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        orna_artifact::client_plan::LANGUAGE_VERSION_IDENTITY,
        &artifact,
        &[],
        &references,
    )
    .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let revision = FunctionRevisionRecord::new(
        STD_CLI_REPL_FUNCTION_ID,
        STD_CLI_REPL_FUNCTION_REVISION_ID,
        STD_CLI_REPL_REVISION_NUMBER,
        function_origin,
        declaration_content_hash,
        semantic_hash,
        orna_artifact::client_plan::LANGUAGE_VERSION_IDENTITY,
        artifact,
    )
    .map_err(|source| StandardLibraryCheckError::Revision { source })?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let executable =
        StandardExecutable::new(STD_CLI_REPL_FUNCTION_ID, revision, references.clone())
            .map_err(|source| StandardLibraryCheckError::Revision { source })?;
    Ok(CheckedStandardExecutable {
        function_id: STD_CLI_REPL_FUNCTION_ID,
        parameter_ids: Vec::new(),
        revision_id: STD_CLI_REPL_FUNCTION_REVISION_ID,
        revision_number: STD_CLI_REPL_REVISION_NUMBER,
        declaration_origin: function_origin,
        declaration_content_hash,
        semantic_hash,
        semantic_hash_version: FunctionSemanticHashVersion::Version2,
        language_version: orna_artifact::client_plan::LANGUAGE_VERSION_IDENTITY.to_owned(),
        artifact: executable.revision().artifact().clone(),
        references,
        schema_origin,
        function_origin,
        parameter_origins: Vec::new(),
    })
}

fn check_standard_library_source_v10(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    let source_units = snapshot.source().units();
    let executables = snapshot.executables();
    if source_units.len() != 10 {
        return Err(StandardLibraryCheckError::SourceUnitCount {
            actual: source_units.len(),
        });
    }
    if executables.len() != 12 {
        return Err(StandardLibraryCheckError::ExecutableCount {
            actual: executables.len(),
        });
    }
    let expected_functions = [
        STD_INVOKE_ECHO_FUNCTION_ID,
        STD_JSON_ENCODE_FUNCTION_ID,
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        STD_UI_WINDOW_FUNCTION_ID,
        STD_UI_TEXT_FUNCTION_ID,
        STD_UI_BUTTON_FUNCTION_ID,
        STD_UI_PANEL_FUNCTION_ID,
        STD_UI_ROW_FUNCTION_ID,
        STD_UI_COLUMN_FUNCTION_ID,
        STD_UI_TEXT_INPUT_FUNCTION_ID,
        STD_UI_TABS_FUNCTION_ID,
        STD_CLI_REPL_FUNCTION_ID,
    ];
    if executables
        .iter()
        .map(StandardExecutable::function)
        .ne(expected_functions)
    {
        return Err(StandardLibraryCheckError::ExecutableMismatch);
    }
    let cli_unit = source_units
        .last()
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let cli_origins = snapshot
        .origins()
        .iter()
        .filter(|origin| origin.source().source_unit() == STD_CLI_SOURCE_UNIT_ID)
        .cloned()
        .collect::<Vec<_>>();
    let parent_origins = snapshot
        .origins()
        .iter()
        .filter(|origin| origin.source().source_unit() != STD_CLI_SOURCE_UNIT_ID)
        .cloned()
        .collect::<Vec<_>>();
    let parent_catalogue = CatalogueSnapshot::new_with_functions_and_types(
        snapshot.catalogue().revision(),
        snapshot
            .catalogue()
            .schemas()
            .iter()
            .filter(|schema| schema.id() != STD_CLI_SCHEMA_ID)
            .cloned()
            .collect(),
        snapshot.catalogue().object_types().to_vec(),
        snapshot.catalogue().value_types().to_vec(),
        snapshot.catalogue().type_bindings().to_vec(),
        snapshot
            .catalogue()
            .functions()
            .iter()
            .filter(|function| function.id() != STD_CLI_REPL_FUNCTION_ID)
            .cloned()
            .collect(),
    )
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let (mut families, mut checked_executables) = check_standard_library_source_v9_parts(
        &source_units[..9],
        &parent_catalogue,
        &parent_origins,
        &executables[..11],
    )?;
    let cli_bundle =
        SourceBundle::new([SourceUnit::new(cli_unit.logical_path(), cli_unit.content())])
            .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let cli_report = parse_bundle(&cli_bundle);
    if !cli_report.diagnostics().is_empty() {
        return Err(StandardLibraryCheckError::Diagnostics {
            diagnostics: cli_report.diagnostics().to_vec(),
        });
    }
    let [parsed_cli] = cli_report.units() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if parsed_cli.source_text() != cli_unit.content()
        || parsed_cli.source_text() != parsed_cli.syntax_text()
        || parsed_cli.parsed().schemas().len() != 1
        || parsed_cli.parsed().client_functions().len() != 1
        || !parsed_cli.parsed().server_functions().is_empty()
        || !parsed_cli.parsed().object_types().is_empty()
        || !parsed_cli.parsed().enum_types().is_empty()
        || !parsed_cli.parsed().primitive_value_types().is_empty()
        || !parsed_cli.parsed().opaque_value_types().is_empty()
        || !parsed_cli.parsed().record_value_types().is_empty()
        || !parsed_cli.parsed().field_renames().is_empty()
        || !parsed_cli.parsed().type_exports().is_empty()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let checked_cli = check_standard_cli_repl(
        &parsed_cli.parsed().client_functions()[0],
        snapshot.catalogue(),
        &cli_origins,
        cli_unit,
    )?;
    families.schemas.push(CheckedStandardSchema {
        id: STD_CLI_SCHEMA_ID,
        name: unquoted_semantic_name(&parsed_cli.parsed().schemas()[0].name)?,
        origin: checked_cli.schema_origin(),
    });
    checked_executables.push(checked_cli);
    Ok(CheckedStandardLibrary {
        verified_snapshot: snapshot.clone(),
        schemas: families.schemas,
        value_types: families.value_types,
        type_bindings: families.type_bindings,
        checked_executables,
    })
}
fn check_standard_library_source_v9_parts(
    source_units: &[StoredSourceUnit],
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, Vec<CheckedStandardExecutable>), StandardLibraryCheckError> {
    let [
        types_unit,
        invoke_unit,
        output_unit,
        ui_unit,
        json_unit,
        action_unit,
        window_unit,
        data_unit,
        constructors_unit,
    ] = source_units
    else {
        return Err(StandardLibraryCheckError::SourceUnitCount {
            actual: source_units.len(),
        });
    };
    check_standard_source_units(
        source_units,
        &[
            (STD_TYPES_SOURCE_UNIT_ID, "std/types.orna", 0),
            (STD_INVOKE_SOURCE_UNIT_ID, "std/invoke.orna", 1),
            (STD_OUTPUT_SOURCE_UNIT_ID, "std/output.orna", 2),
            (STD_UI_SOURCE_UNIT_ID, "std/ui.orna", 3),
            (STD_JSON_SOURCE_UNIT_ID, "std/json.orna", 4),
            (STD_ACTION_SOURCE_UNIT_ID, "std/action.orna", 5),
            (STD_WINDOW_SOURCE_UNIT_ID, "std/window.orna", 6),
            (STD_DATA_SOURCE_UNIT_ID, "std/data.orna", 7),
            (
                STD_UI_CONSTRUCTORS_SOURCE_UNIT_ID,
                "std/ui_constructors.orna",
                8,
            ),
        ],
    )?;
    if executables.len() != 11 {
        return Err(StandardLibraryCheckError::ExecutableCount {
            actual: executables.len(),
        });
    }
    let expected_functions = [
        STD_INVOKE_ECHO_FUNCTION_ID,
        STD_JSON_ENCODE_FUNCTION_ID,
        STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
        STD_UI_WINDOW_FUNCTION_ID,
        STD_UI_TEXT_FUNCTION_ID,
        STD_UI_BUTTON_FUNCTION_ID,
        STD_UI_PANEL_FUNCTION_ID,
        STD_UI_ROW_FUNCTION_ID,
        STD_UI_COLUMN_FUNCTION_ID,
        STD_UI_TEXT_INPUT_FUNCTION_ID,
        STD_UI_TABS_FUNCTION_ID,
    ];
    if executables.len() != expected_functions.len()
        || executables
            .iter()
            .map(StandardExecutable::function)
            .ne(expected_functions)
    {
        return Err(StandardLibraryCheckError::ExecutableMismatch);
    }

    let mut v8_origins = Vec::with_capacity(origins.len());
    let mut constructor_origins = Vec::new();
    for origin in origins {
        if origin.source().source_unit() == STD_UI_CONSTRUCTORS_SOURCE_UNIT_ID {
            constructor_origins.push(origin.clone());
        } else {
            v8_origins.push(origin.clone());
        }
    }
    let v8_functions = catalogue
        .functions()
        .iter()
        .filter(|function| expected_functions[..4].contains(&function.id()))
        .cloned()
        .collect::<Vec<_>>();
    if v8_functions.len() != 4 {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let v8_catalogue = CatalogueSnapshot::new_with_functions_and_types(
        catalogue.revision(),
        catalogue.schemas().to_vec(),
        catalogue.object_types().to_vec(),
        catalogue.value_types().to_vec(),
        catalogue.type_bindings().to_vec(),
        v8_functions,
    )
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let (families, mut checked_executables) = check_standard_library_source_v8_parts(
        &[
            types_unit.clone(),
            invoke_unit.clone(),
            output_unit.clone(),
            ui_unit.clone(),
            json_unit.clone(),
            action_unit.clone(),
            window_unit.clone(),
            data_unit.clone(),
        ],
        &v8_catalogue,
        &v8_origins,
        &executables[..4],
    )?;

    let constructors_bundle = SourceBundle::new([SourceUnit::new(
        constructors_unit.logical_path(),
        constructors_unit.content(),
    )])
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let constructors_report = parse_bundle(&constructors_bundle);
    if !constructors_report.diagnostics().is_empty() {
        return Err(StandardLibraryCheckError::Diagnostics {
            diagnostics: constructors_report.diagnostics().to_vec(),
        });
    }
    let [parsed_constructors] = constructors_report.units() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if parsed_constructors.source_text() != constructors_unit.content()
        || parsed_constructors.source_text() != parsed_constructors.syntax_text()
        || !parsed_constructors.parsed().schemas().is_empty()
        || !parsed_constructors.parsed().object_types().is_empty()
        || !parsed_constructors.parsed().enum_types().is_empty()
        || !parsed_constructors
            .parsed()
            .primitive_value_types()
            .is_empty()
        || !parsed_constructors.parsed().opaque_value_types().is_empty()
        || !parsed_constructors.parsed().record_value_types().is_empty()
        || !parsed_constructors.parsed().field_renames().is_empty()
        || !parsed_constructors.parsed().type_exports().is_empty()
        || !parsed_constructors.parsed().server_functions().is_empty()
        || parsed_constructors.parsed().client_functions().len() != 7
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let ui_schema_origin = v8_origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Schema(STD_UI_SCHEMA_ID))
        .map(DefinitionOrigin::source)
        .ok_or(StandardLibraryCheckError::MissingSchemaOrigin)?;

    for (index, declaration) in parsed_constructors
        .parsed()
        .client_functions()
        .iter()
        .enumerate()
    {
        let expected_name = unquoted_semantic_name(&declaration.name)?;
        let spec = standard_ui_constructor_spec(&expected_name)
            .ok_or(StandardLibraryCheckError::SourceMismatch)?;
        if spec.function_id != expected_functions[index + 4] {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
        let declaration_origins = constructor_origins
            .iter()
            .filter(|origin| match origin.identity() {
                DefinitionIdentity::Function(function) => function == spec.function_id,
                DefinitionIdentity::Parameter { owner, .. } => owner == spec.function_id,
                _ => false,
            })
            .cloned()
            .collect::<Vec<_>>();
        let checked = check_standard_ui_constructor(declaration, catalogue, &declaration_origins)?;
        let checked_executable = reconcile_standard_ui_constructor_executable(
            catalogue,
            &declaration_origins,
            &executables[index + 4],
            constructors_unit,
            declaration,
            checked,
            ui_schema_origin,
        )?;
        checked_executables.push(checked_executable);
    }
    if constructor_origins.len()
        != parsed_constructors
            .parsed()
            .client_functions()
            .iter()
            .map(|declaration| declaration.parameters.len() + 1)
            .sum::<usize>()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    Ok((families, checked_executables))
}
/// Reconciles the appended `std/data.orna` unit and returns its catalogue
/// families. The table declaration is checked through the shared retained
/// terminal-table checker below; this function additionally requires the
/// complete source-owned origin set and the cross-unit terminal reference.
fn reconcile_standard_data_unit(
    stored_unit: &StoredSourceUnit,
    parsed_unit: &ParsedSourceUnit,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
) -> Result<StandardSourceFamilies, StandardLibraryCheckError> {
    if parsed_unit.source_text() != stored_unit.content()
        || parsed_unit.source_text() != parsed_unit.syntax_text()
        || !parsed_unit.parsed().object_types().is_empty()
        || !parsed_unit.parsed().enum_types().is_empty()
        || !parsed_unit.parsed().primitive_value_types().is_empty()
        || !parsed_unit.parsed().record_value_types().is_empty()
        || !parsed_unit.parsed().field_renames().is_empty()
        || !parsed_unit.parsed().client_functions().is_empty()
        || parsed_unit.parsed().schemas().len() != 1
        || parsed_unit.parsed().opaque_value_types().len() != 1
        || parsed_unit.parsed().type_exports().len() != 1
        || parsed_unit.parsed().server_functions().len() != 1
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let [schema_declaration] = parsed_unit.parsed().schemas() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [rows_type_declaration] = parsed_unit.parsed().opaque_value_types() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [rows_export] = parsed_unit.parsed().type_exports() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [table_function] = parsed_unit.parsed().server_functions() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };

    let expected_schema_name =
        QualifiedSemanticName::new(["std", "data"]).expect("fixed data schema is valid");
    let schema_name = unquoted_semantic_name(&schema_declaration.name)?;
    if schema_name != expected_schema_name
        || catalogue
            .schema_by_id(STD_DATA_SCHEMA_ID)
            .ok_or(StandardLibraryCheckError::MissingSchema)?
            .name()
            != &schema_name
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let expected_rows_name =
        QualifiedSemanticName::new(["std", "data", "rows"]).expect("fixed Rows type is valid");
    let rows_name = unquoted_semantic_name(&rows_type_declaration.name)?;
    let rows_contract = decode_string_literal(&rows_type_declaration.kernel_contract)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let rows_definition = catalogue
        .value_type_by_id(STD_DATA_ROWS_TYPE_ID)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    if rows_name != expected_rows_name
        || rows_contract != "orna.std.value.rows@1"
        || rows_definition.name() != &rows_name
        || rows_definition.kind() != ValueTypeKind::Opaque
        || rows_definition.mutability() != ValueTypeMutability::Immutable
        || rows_definition.persistence() != ValueTypePersistence::Transient
        || rows_definition.representation_contract() != rows_contract
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let expected_binding_name =
        QualifiedSemanticName::new(["std", "rows"]).expect("fixed Rows export is valid");
    let rows_binding = catalogue
        .type_binding_by_name(&TypeLookupName::qualified(expected_binding_name.clone()))
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let TypeExportTarget::Qualified { name: target_name } = &rows_export.target else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if unquoted_semantic_name(&rows_export.source_type)? != rows_name
        || unquoted_semantic_name(target_name)? != expected_binding_name
        || rows_binding.id() != STD_DATA_ROWS_TYPE_BINDING_ID
        || rows_binding.kind() != TypeBindingKind::Qualified
        || rows_binding.name() != &TypeLookupName::qualified(expected_binding_name)
        || rows_binding.target() != STD_DATA_ROWS_TYPE_ID
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let mut origins_by_identity = HashMap::with_capacity(origins.len());
    for origin in origins {
        if !matches!(
            origin.identity(),
            DefinitionIdentity::Schema(_)
                | DefinitionIdentity::ValueType(_)
                | DefinitionIdentity::TypeBinding(_)
                | DefinitionIdentity::Function(_)
                | DefinitionIdentity::Parameter { .. }
        ) || origins_by_identity
            .insert(origin.identity(), origin.source())
            .is_some()
        {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
    }
    let schema_origin = take_origin(
        &mut origins_by_identity,
        DefinitionIdentity::Schema(STD_DATA_SCHEMA_ID),
        stored_unit.id(),
        &schema_declaration.span,
    )?;
    let rows_origin = take_origin(
        &mut origins_by_identity,
        DefinitionIdentity::ValueType(STD_DATA_ROWS_TYPE_ID),
        stored_unit.id(),
        &rows_type_declaration.span,
    )?;
    let binding_origin = take_origin(
        &mut origins_by_identity,
        DefinitionIdentity::TypeBinding(rows_binding.id()),
        stored_unit.id(),
        &rows_export.span,
    )?;
    let function_origin = take_origin(
        &mut origins_by_identity,
        DefinitionIdentity::Function(STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID),
        stored_unit.id(),
        &table_function.span,
    )?;
    let parameter = table_function
        .parameters
        .first()
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let parameter_origin = take_origin(
        &mut origins_by_identity,
        DefinitionIdentity::Parameter {
            owner: STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
            parameter: STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
        },
        stored_unit.id(),
        &parameter.span,
    )?;
    if !origins_by_identity.is_empty()
        || schema_origin.source_unit() != stored_unit.id()
        || rows_origin.source_unit() != stored_unit.id()
        || binding_origin.source_unit() != stored_unit.id()
        || function_origin.source_unit() != stored_unit.id()
        || parameter_origin.source_unit() != stored_unit.id()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    check_standard_terminal_present_table(
        table_function,
        catalogue,
        origins,
        STD_DATA_ROWS_TYPE_ID,
    )?;
    Ok(StandardSourceFamilies {
        schemas: vec![CheckedStandardSchema {
            id: STD_DATA_SCHEMA_ID,
            name: schema_name,
            origin: schema_origin,
        }],
        value_types: vec![CheckedStandardValueType {
            id: STD_DATA_ROWS_TYPE_ID,
            name: rows_name,
            kind: rows_definition.kind(),
            mutability: rows_definition.mutability(),
            persistence: rows_definition.persistence(),
            representation_contract: rows_definition.representation_contract().to_owned(),
            origin: rows_origin,
        }],
        type_bindings: vec![CheckedStandardTypeBinding {
            id: rows_binding.id(),
            kind: rows_binding.kind(),
            name: rows_binding.name().clone(),
            target: rows_binding.target(),
            origin: binding_origin,
        }],
    })
}

pub(super) fn check_standard_library_source_v6_parts(
    source_units: &[StoredSourceUnit],
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    let [
        types_unit,
        invoke_unit,
        output_unit,
        ui_unit,
        json_unit,
        action_unit,
    ] = source_units
    else {
        return Err(StandardLibraryCheckError::SourceUnitCount {
            actual: source_units.len(),
        });
    };
    check_standard_source_units(
        source_units,
        &[
            (STD_TYPES_SOURCE_UNIT_ID, "std/types.orna", 0),
            (STD_INVOKE_SOURCE_UNIT_ID, "std/invoke.orna", 1),
            (STD_OUTPUT_SOURCE_UNIT_ID, "std/output.orna", 2),
            (STD_UI_SOURCE_UNIT_ID, "std/ui.orna", 3),
            (STD_JSON_SOURCE_UNIT_ID, "std/json.orna", 4),
            (STD_ACTION_SOURCE_UNIT_ID, "std/action.orna", 5),
        ],
    )?;

    let mut v5_origins = Vec::with_capacity(origins.len());
    let mut action_origins = Vec::new();
    for origin in origins {
        if origin.source().source_unit() == STD_ACTION_SOURCE_UNIT_ID {
            action_origins.push(origin.clone());
        } else {
            v5_origins.push(origin.clone());
        }
    }
    let (mut families, checked_executable) = check_standard_library_source_v5_parts(
        &[
            types_unit.clone(),
            invoke_unit.clone(),
            output_unit.clone(),
            ui_unit.clone(),
            json_unit.clone(),
        ],
        catalogue,
        &v5_origins,
        executables,
    )?;
    let bundle = SourceBundle::new([SourceUnit::new(
        action_unit.logical_path(),
        action_unit.content(),
    )])
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let report = parse_bundle(&bundle);
    if !report.diagnostics().is_empty() {
        return Err(StandardLibraryCheckError::Diagnostics {
            diagnostics: report.diagnostics().to_vec(),
        });
    }
    let [parsed_action] = report.units() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let action_families =
        reconcile_standard_action_unit(action_unit, parsed_action, catalogue, &action_origins)?;
    families.schemas.extend(action_families.schemas);
    families.value_types.extend(action_families.value_types);
    families.type_bindings.extend(action_families.type_bindings);

    Ok((families, checked_executable))
}

pub(super) fn check_standard_library_source_v5_parts(
    source_units: &[StoredSourceUnit],
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    let [types_unit, invoke_unit, output_unit, ui_unit, json_unit] = source_units else {
        return Err(StandardLibraryCheckError::SourceUnitCount {
            actual: source_units.len(),
        });
    };
    check_standard_source_units(
        source_units,
        &[
            (STD_TYPES_SOURCE_UNIT_ID, "std/types.orna", 0),
            (STD_INVOKE_SOURCE_UNIT_ID, "std/invoke.orna", 1),
            (STD_OUTPUT_SOURCE_UNIT_ID, "std/output.orna", 2),
            (STD_UI_SOURCE_UNIT_ID, "std/ui.orna", 3),
            (STD_JSON_SOURCE_UNIT_ID, "std/json.orna", 4),
        ],
    )?;
    let bundle = SourceBundle::new([
        SourceUnit::new(types_unit.logical_path(), types_unit.content()),
        SourceUnit::new(invoke_unit.logical_path(), invoke_unit.content()),
        SourceUnit::new(output_unit.logical_path(), output_unit.content()),
        SourceUnit::new(ui_unit.logical_path(), ui_unit.content()),
        SourceUnit::new(json_unit.logical_path(), json_unit.content()),
    ])
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let report = parse_bundle(&bundle);
    if !report.diagnostics().is_empty() {
        return Err(StandardLibraryCheckError::Diagnostics {
            diagnostics: report.diagnostics().to_vec(),
        });
    }
    let [
        parsed_types,
        parsed_invoke,
        parsed_output,
        parsed_ui,
        parsed_json,
    ] = report.units()
    else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };

    let mut retained_v4_origins = Vec::with_capacity(origins.len());
    let mut json_origins = Vec::new();
    for origin in origins {
        if origin.source().source_unit() == STD_JSON_SOURCE_UNIT_ID {
            json_origins.push(origin.clone());
        } else {
            retained_v4_origins.push(origin.clone());
        }
    }
    let origin_partitions = partition_standard_v4_origins(&retained_v4_origins)?;
    let types_catalogue = standard_v5_types_catalogue(catalogue)?;
    let families = reconcile_standard_source(
        types_unit,
        parsed_types,
        &types_catalogue,
        &origin_partitions.0,
    )?;
    let Some(echo_executable) = executables
        .iter()
        .find(|executable| executable.function() == STD_INVOKE_ECHO_FUNCTION_ID)
    else {
        return Err(StandardLibraryCheckError::ExecutableMismatch);
    };
    let Some(json_executable) = executables
        .iter()
        .find(|executable| executable.function() == STD_JSON_ENCODE_FUNCTION_ID)
    else {
        return Err(StandardLibraryCheckError::ExecutableMismatch);
    };
    if executables.len() != 2 {
        return Err(StandardLibraryCheckError::ExecutableCount {
            actual: executables.len(),
        });
    }
    let checked_executable = reconcile_standard_invoke_executable(
        catalogue,
        &origin_partitions.1,
        std::slice::from_ref(echo_executable),
        invoke_unit,
        parsed_invoke,
    )?;
    reconcile_standard_output_unit(output_unit, parsed_output, catalogue, &origin_partitions.2)?;
    reconcile_standard_ui_unit(ui_unit, parsed_ui, catalogue, &origin_partitions.3)?;
    reconcile_standard_json_unit(json_unit, parsed_json, catalogue, &json_origins)?;
    let [json_function] = parsed_json.parsed().server_functions() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    reconcile_standard_json_executable(
        json_executable,
        json_function,
        catalogue,
        &json_origins,
        json_unit,
    )?;
    Ok((families, checked_executable))
}

pub(super) fn expected_standard_json_executable(
    declaration: &ServerFunctionDeclaration,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    stored_unit: &StoredSourceUnit,
) -> Result<StandardExecutable, StandardLibraryCheckError> {
    check_standard_json_encode(declaration, catalogue, origins, STD_JSON_VALUE_TYPE_ID)?;
    let function_origin = origins
        .iter()
        .find(|origin| {
            origin.identity() == DefinitionIdentity::Function(STD_JSON_ENCODE_FUNCTION_ID)
        })
        .ok_or(StandardLibraryCheckError::PresenterMissingFunctionOrigin)?
        .source();
    let declaration_bytes = &stored_unit.content().as_bytes()
        [function_origin.byte_start() as usize..function_origin.byte_end() as usize];
    let declaration_content_hash = function_declaration_digest(declaration_bytes)
        .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let function = catalogue
        .function_by_id(STD_JSON_ENCODE_FUNCTION_ID)
        .ok_or(StandardLibraryCheckError::PresenterMissingFunction)?;
    let payload = JsonEncodePlan::new(STD_JSON_ENCODE_PARAMETER_ID, STD_JSON_VALUE_TYPE_ID)
        .expect("fixed JSON presenter identities are valid")
        .encode()
        .expect("the fixed JSON presenter payload is within the format limit");
    let artifact_hash = artifact_payload_digest(&payload)
        .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        server_json_encode::FORMAT_IDENTITY,
        server_json_encode::FORMAT_VERSION,
        payload,
        artifact_hash,
    )
    .map_err(|source| StandardLibraryCheckError::Revision { source })?;
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        server_json_encode::LANGUAGE_VERSION_IDENTITY,
        &artifact,
        &[],
        &[],
    )
    .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let revision = FunctionRevisionRecord::new(
        STD_JSON_ENCODE_FUNCTION_ID,
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        u64::from(server_json_encode::FORMAT_VERSION),
        function_origin,
        declaration_content_hash,
        semantic_hash,
        server_json_encode::LANGUAGE_VERSION_IDENTITY,
        artifact,
    )
    .map_err(|source| StandardLibraryCheckError::Revision { source })?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    StandardExecutable::new(STD_JSON_ENCODE_FUNCTION_ID, revision, Vec::new())
        .map_err(|source| StandardLibraryCheckError::Revision { source })
}

fn expected_standard_terminal_executable(
    declaration: &ServerFunctionDeclaration,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    stored_unit: &StoredSourceUnit,
) -> Result<StandardExecutable, StandardLibraryCheckError> {
    let checked = check_standard_terminal_present_table(
        declaration,
        catalogue,
        origins,
        STD_DATA_ROWS_TYPE_ID,
    )?;
    let function_origin = origins
        .iter()
        .find(|origin| {
            origin.identity()
                == DefinitionIdentity::Function(STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID)
        })
        .ok_or(StandardLibraryCheckError::PresenterMissingFunctionOrigin)?
        .source();
    let declaration_bytes = &stored_unit.content().as_bytes()
        [function_origin.byte_start() as usize..function_origin.byte_end() as usize];
    let declaration_content_hash = function_declaration_digest(declaration_bytes)
        .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let function = catalogue
        .function_by_id(STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID)
        .ok_or(StandardLibraryCheckError::PresenterMissingFunction)?;
    let payload = server_terminal_table::TerminalTablePlan::new(
        STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
        STD_DATA_ROWS_TYPE_ID,
    )
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?
    .encode()
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let artifact_hash = artifact_payload_digest(&payload)
        .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        server_terminal_table::FORMAT_IDENTITY,
        server_terminal_table::FORMAT_VERSION,
        payload,
        artifact_hash,
    )
    .map_err(|source| StandardLibraryCheckError::Revision { source })?;
    let source_origin = |span: &SourceSpan| -> Result<SourceOrigin, StandardLibraryCheckError> {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
        let end = u32::try_from(span.end).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
        SourceOrigin::new(stored_unit.id(), start, end)
            .map_err(|_| StandardLibraryCheckError::SourceMismatch)
    };
    let parameter = declaration
        .parameters
        .first()
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let result = match &declaration.return_type {
        FunctionReturnType::Single(result) => result,
        FunctionReturnType::Rows { .. } | FunctionReturnType::Stream { .. } => {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
    };
    let body = declaration
        .body
        .as_no_input_parameter_select()
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let references = vec![
        DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            0,
            DefinitionReferenceTarget::ValueType(STD_DATA_ROWS_TYPE_ID),
            DefinitionReferenceKind::NamedType,
            source_origin(parameter.type_specification.span())?,
        ),
        DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            1,
            DefinitionReferenceTarget::ValueType(STD_TERMINAL_DOCUMENT_TYPE_ID),
            DefinitionReferenceKind::NamedType,
            source_origin(result.span())?,
        ),
        DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            2,
            DefinitionReferenceTarget::Parameter {
                owner: STD_TERMINAL_PRESENT_TABLE_FUNCTION_ID,
                parameter: STD_TERMINAL_PRESENT_TABLE_PARAMETER_ID,
            },
            DefinitionReferenceKind::ParameterRead,
            source_origin(&body.parameter.span)?,
        ),
    ];
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        server_terminal_table::LANGUAGE_VERSION_IDENTITY,
        &artifact,
        &[],
        &references,
    )
    .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let revision = FunctionRevisionRecord::new(
        checked.function_id(),
        checked.revision_id(),
        u64::from(server_terminal_table::FORMAT_VERSION),
        function_origin,
        declaration_content_hash,
        semantic_hash,
        server_terminal_table::LANGUAGE_VERSION_IDENTITY,
        artifact,
    )
    .map_err(|source| StandardLibraryCheckError::Revision { source })?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    StandardExecutable::new(checked.function_id(), revision, references)
        .map_err(|source| StandardLibraryCheckError::Revision { source })
}

fn reconcile_standard_terminal_executable(
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    stored: &StandardExecutable,
    stored_unit: &StoredSourceUnit,
    parsed_unit: &ParsedSourceUnit,
    schema_origin: SourceOrigin,
) -> Result<CheckedStandardExecutable, StandardLibraryCheckError> {
    if parsed_unit.source_text() != stored_unit.content()
        || parsed_unit.source_text() != parsed_unit.syntax_text()
        || parsed_unit.parsed().schemas().len() != 1
        || !parsed_unit.parsed().object_types().is_empty()
        || !parsed_unit.parsed().enum_types().is_empty()
        || !parsed_unit.parsed().primitive_value_types().is_empty()
        || parsed_unit.parsed().opaque_value_types().len() != 1
        || !parsed_unit.parsed().record_value_types().is_empty()
        || !parsed_unit.parsed().field_renames().is_empty()
        || !parsed_unit.parsed().client_functions().is_empty()
        || parsed_unit.parsed().server_functions().len() != 1
        || parsed_unit.parsed().type_exports().len() != 1
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let [declaration] = parsed_unit.parsed().server_functions() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let expected =
        expected_standard_terminal_executable(declaration, catalogue, origins, stored_unit)?;
    if stored != &expected {
        return Err(StandardLibraryCheckError::ExecutableMismatch);
    }
    checked_standard_executable_from_record(&expected, catalogue, origins, schema_origin)
}

pub(super) fn reconcile_standard_json_executable(
    stored: &StandardExecutable,
    declaration: &ServerFunctionDeclaration,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    stored_unit: &StoredSourceUnit,
) -> Result<(), StandardLibraryCheckError> {
    let expected = expected_standard_json_executable(declaration, catalogue, origins, stored_unit)?;
    if stored != &expected {
        return Err(StandardLibraryCheckError::ExecutableMismatch);
    }
    Ok(())
}

fn checked_standard_executable_from_record(
    record: &StandardExecutable,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    schema_origin: SourceOrigin,
) -> Result<CheckedStandardExecutable, StandardLibraryCheckError> {
    let function_id = record.function();
    let function = catalogue
        .function_by_id(function_id)
        .ok_or(StandardLibraryCheckError::MissingFunction)?;
    let parameter_ids = function
        .parameters()
        .iter()
        .map(|parameter| parameter.id())
        .collect::<Vec<_>>();
    let parameter_origins = parameter_ids
        .iter()
        .map(|parameter| {
            origins
                .iter()
                .find(|origin| {
                    origin.identity()
                        == DefinitionIdentity::Parameter {
                            owner: function_id,
                            parameter: *parameter,
                        }
                })
                .ok_or(StandardLibraryCheckError::PresenterMissingParameterOrigin)
                .map(DefinitionOrigin::source)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let function_origin = origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Function(function_id))
        .ok_or(StandardLibraryCheckError::PresenterMissingFunctionOrigin)?
        .source();
    let revision = record.revision();
    Ok(CheckedStandardExecutable {
        function_id,
        parameter_ids,
        revision_id: revision.id(),
        revision_number: revision.revision_number(),
        declaration_origin: revision.declaration_origin(),
        declaration_content_hash: revision.declaration_content_hash(),
        semantic_hash: revision.semantic_hash(),
        semantic_hash_version: revision.semantic_hash_version(),
        language_version: revision.language_version().to_owned(),
        artifact: revision.artifact().clone(),
        references: record.references().to_vec(),
        parameter_origins,
        schema_origin,
        function_origin,
    })
}

fn reconcile_standard_ui_constructor_executable(
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    stored: &StandardExecutable,
    stored_unit: &StoredSourceUnit,
    declaration: &ClientFunctionDeclaration,
    checked: CheckedStandardUiConstructor,
    schema_origin: SourceOrigin,
) -> Result<CheckedStandardExecutable, StandardLibraryCheckError> {
    let function_origin = origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Function(checked.function_id()))
        .ok_or(StandardLibraryCheckError::MissingFunctionOrigin)?
        .source();
    let source_origin = |span: &SourceSpan| -> Result<SourceOrigin, StandardLibraryCheckError> {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
        let end = u32::try_from(span.end).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
        SourceOrigin::new(stored_unit.id(), start, end)
            .map_err(|_| StandardLibraryCheckError::SourceMismatch)
    };
    let mut references = Vec::with_capacity(declaration.parameters.len() + 1);
    for (ordinal, parameter) in declaration.parameters.iter().enumerate() {
        let target = resolved_standard_type_id(&parameter.type_specification, catalogue)
            .ok_or(StandardLibraryCheckError::SourceMismatch)?;
        references.push(DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            ordinal as u32,
            DefinitionReferenceTarget::ValueType(target),
            DefinitionReferenceKind::NamedType,
            source_origin(parameter.type_specification.span())?,
        ));
    }
    let FunctionReturnType::Single(result_type) = &declaration.return_type else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    references.push(DefinitionReference::new(
        checked.function_id(),
        checked.revision_id(),
        declaration.parameters.len() as u32,
        DefinitionReferenceTarget::ValueType(STD_UI_TYPE_ID),
        DefinitionReferenceKind::NamedType,
        source_origin(result_type.span())?,
    ));
    let plan = ExpressionClientPlan::new(ClientExpressionNode::ExternalContract {
        identity: checked.runtime_contract().to_owned(),
    });
    let payload = plan
        .encode()
        .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let artifact_hash = artifact_payload_digest(&payload)
        .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        CLIENT_PLAN_FORMAT,
        plan.format_version(),
        payload,
        artifact_hash,
    )
    .map_err(|source| StandardLibraryCheckError::Revision { source })?;
    let function = catalogue
        .function_by_id(checked.function_id())
        .ok_or(StandardLibraryCheckError::MissingFunction)?;
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        orna_artifact::client_plan::LANGUAGE_VERSION_IDENTITY,
        &artifact,
        &[],
        &references,
    )
    .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let declaration_bytes = &stored_unit.content().as_bytes()
        [function_origin.byte_start() as usize..function_origin.byte_end() as usize];
    let declaration_content_hash = function_declaration_digest(declaration_bytes)
        .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let revision = FunctionRevisionRecord::new(
        checked.function_id(),
        checked.revision_id(),
        1,
        function_origin,
        declaration_content_hash,
        semantic_hash,
        orna_artifact::client_plan::LANGUAGE_VERSION_IDENTITY,
        artifact,
    )
    .map_err(|source| StandardLibraryCheckError::Revision { source })?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let expected = StandardExecutable::new(checked.function_id(), revision, references)
        .map_err(|source| StandardLibraryCheckError::Revision { source })?;
    if stored != &expected {
        return Err(StandardLibraryCheckError::ExecutableMismatch);
    }
    checked_standard_executable_from_record(&expected, catalogue, origins, schema_origin)
}
fn reconcile_standard_window_executable(
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    stored: &StandardExecutable,
    stored_unit: &StoredSourceUnit,
    parsed_unit: &ParsedSourceUnit,
    schema_origin: SourceOrigin,
) -> Result<CheckedStandardExecutable, StandardLibraryCheckError> {
    if parsed_unit.source_text() != stored_unit.content()
        || parsed_unit.source_text() != parsed_unit.syntax_text()
        || !parsed_unit.parsed().schemas().is_empty()
        || !parsed_unit.parsed().object_types().is_empty()
        || !parsed_unit.parsed().enum_types().is_empty()
        || !parsed_unit.parsed().primitive_value_types().is_empty()
        || !parsed_unit.parsed().opaque_value_types().is_empty()
        || !parsed_unit.parsed().record_value_types().is_empty()
        || !parsed_unit.parsed().field_renames().is_empty()
        || !parsed_unit.parsed().server_functions().is_empty()
        || parsed_unit.parsed().client_functions().len() != 1
        || !parsed_unit.parsed().type_exports().is_empty()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let [declaration] = parsed_unit.parsed().client_functions() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let checked = check_standard_ui_window(declaration, catalogue, origins)?;
    let function_origin = origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Function(STD_UI_WINDOW_FUNCTION_ID))
        .ok_or(StandardLibraryCheckError::MissingFunctionOrigin)?
        .source();
    let source_origin = |span: &SourceSpan| -> Result<SourceOrigin, StandardLibraryCheckError> {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
        let end = u32::try_from(span.end).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
        SourceOrigin::new(stored_unit.id(), start, end)
            .map_err(|_| StandardLibraryCheckError::SourceMismatch)
    };
    let title_type_origin = source_origin(declaration.parameters[0].type_specification.span())?;
    let content_type_origin = source_origin(declaration.parameters[1].type_specification.span())?;
    let result_type = match &declaration.return_type {
        FunctionReturnType::Single(result) => result,
        FunctionReturnType::Rows { .. } | FunctionReturnType::Stream { .. } => {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
    };
    let result_type_origin = source_origin(result_type.span())?;
    let references = vec![
        DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            0,
            DefinitionReferenceTarget::ValueType(STD_CHARACTER_LARGE_OBJECT_TYPE_ID),
            DefinitionReferenceKind::NamedType,
            title_type_origin,
        ),
        DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            1,
            DefinitionReferenceTarget::ValueType(STD_UI_TYPE_ID),
            DefinitionReferenceKind::NamedType,
            content_type_origin,
        ),
        DefinitionReference::new(
            checked.function_id(),
            checked.revision_id(),
            2,
            DefinitionReferenceTarget::ValueType(STD_UI_TYPE_ID),
            DefinitionReferenceKind::NamedType,
            result_type_origin,
        ),
    ];
    let plan = ExpressionClientPlan::new(ClientExpressionNode::ExternalContract {
        identity: STD_UI_WINDOW_RUNTIME_CONTRACT.to_owned(),
    });
    let payload = plan
        .encode()
        .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let artifact_hash = artifact_payload_digest(&payload)
        .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Client,
        CLIENT_PLAN_FORMAT,
        plan.format_version(),
        payload,
        artifact_hash,
    )
    .map_err(|source| StandardLibraryCheckError::Revision { source })?;
    let function = catalogue
        .function_by_id(STD_UI_WINDOW_FUNCTION_ID)
        .ok_or(StandardLibraryCheckError::MissingFunction)?;
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        orna_artifact::client_plan::LANGUAGE_VERSION_IDENTITY,
        &artifact,
        &[],
        &references,
    )
    .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let declaration_bytes = &stored_unit.content().as_bytes()
        [function_origin.byte_start() as usize..function_origin.byte_end() as usize];
    let declaration_content_hash = function_declaration_digest(declaration_bytes)
        .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let revision = FunctionRevisionRecord::new(
        checked.function_id(),
        checked.revision_id(),
        STD_UI_WINDOW_REVISION_NUMBER,
        function_origin,
        declaration_content_hash,
        semantic_hash,
        orna_artifact::client_plan::LANGUAGE_VERSION_IDENTITY,
        artifact,
    )
    .map_err(|source| StandardLibraryCheckError::Revision { source })?
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let expected = StandardExecutable::new(checked.function_id(), revision, references)
        .map_err(|source| StandardLibraryCheckError::Revision { source })?;
    if stored != &expected {
        return Err(StandardLibraryCheckError::ExecutableMismatch);
    }
    let mut checked_executable =
        checked_standard_executable_from_record(&expected, catalogue, origins, schema_origin)?;
    checked_executable.parameter_ids =
        vec![checked.title_parameter_id(), checked.content_parameter_id()];
    Ok(checked_executable)
}

/// Scopes one standard catalogue to the declarations retained in one source
/// unit, dropping the excluded schemas and value types and every type binding
/// that targets an excluded value type.
///
/// The returned scope carries no functions; the invoke path reconciles the
/// executable functions separately. Object, enum, and record value types are
/// not part of any retained standard source and fail closed.
fn scope_standard_catalogue(
    catalogue: &CatalogueSnapshot,
    excluded_schemas: &[SchemaId],
    excluded_value_types: &[TypeId],
) -> Result<CatalogueSnapshot, StandardLibraryCheckError> {
    if !catalogue.object_types().is_empty()
        || !catalogue.enum_types().is_empty()
        || !catalogue.record_value_types().is_empty()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let mut schemas = Vec::with_capacity(catalogue.schemas().len());
    for schema in catalogue.schemas() {
        if excluded_schemas.contains(&schema.id()) {
            continue;
        }
        schemas.push(schema.clone());
    }
    let mut value_types = Vec::with_capacity(catalogue.value_types().len());
    for value_type in catalogue.value_types() {
        if excluded_value_types.contains(&value_type.id()) {
            continue;
        }
        value_types.push(value_type.clone());
    }
    let mut type_bindings = Vec::with_capacity(catalogue.type_bindings().len());
    for binding in catalogue.type_bindings() {
        if excluded_value_types.contains(&binding.target()) {
            continue;
        }
        type_bindings.push(binding.clone());
    }
    CatalogueSnapshot::new_with_functions_and_types(
        catalogue.revision(),
        schemas,
        vec![],
        value_types,
        type_bindings,
        vec![],
    )
    .map_err(|_| StandardLibraryCheckError::SourceMismatch)
}

/// Scopes the V3 catalogue to the declarations retained in `std/types.orna`:
/// the standard schemas, value types, and type bindings only.
///
/// The `std.invoke`, `std.terminal`, and `std.io` schemas, the two opaque
/// output value types, and their exports are declared in the other retained
/// units and are reconciled by their own paths; the V1 type-only reconcile
/// contract must not see them. Any other catalogue schema, function, object,
/// enum, or record type fails closed.
fn standard_v3_types_catalogue(
    catalogue: &CatalogueSnapshot,
) -> Result<CatalogueSnapshot, StandardLibraryCheckError> {
    scope_standard_catalogue(
        catalogue,
        &[
            STD_INVOKE_SCHEMA_ID,
            STD_TERMINAL_SCHEMA_ID,
            STD_IO_SCHEMA_ID,
        ],
        &[STD_TERMINAL_DOCUMENT_TYPE_ID, STD_IO_BYTE_STREAM_TYPE_ID],
    )
}

/// Scopes the V4 catalogue to the declarations retained in `std/types.orna`:
/// the standard schemas, value types, and type bindings only.
///
/// The `std.invoke`, `std.terminal`, `std.io`, and `std.ui` schemas, the
/// three opaque output and ui value types, and their exports are declared in
/// the other retained units and are reconciled by their own paths; the V1
/// type-only reconcile contract must not see them. Any other catalogue
/// schema, function, object, enum, or record type fails closed.
fn standard_v4_types_catalogue(
    catalogue: &CatalogueSnapshot,
) -> Result<CatalogueSnapshot, StandardLibraryCheckError> {
    scope_standard_catalogue(
        catalogue,
        &[
            STD_INVOKE_SCHEMA_ID,
            STD_TERMINAL_SCHEMA_ID,
            STD_IO_SCHEMA_ID,
            STD_UI_SCHEMA_ID,
        ],
        &[
            STD_TERMINAL_DOCUMENT_TYPE_ID,
            STD_IO_BYTE_STREAM_TYPE_ID,
            STD_UI_TYPE_ID,
        ],
    )
}

/// Scopes the V5 and V6 catalogues to declarations retained in `std/types.orna`.
/// The JSON and action schemas and value types are reconciled in their own units.
fn standard_v5_types_catalogue(
    catalogue: &CatalogueSnapshot,
) -> Result<CatalogueSnapshot, StandardLibraryCheckError> {
    scope_standard_catalogue(
        catalogue,
        &[
            STD_INVOKE_SCHEMA_ID,
            STD_TERMINAL_SCHEMA_ID,
            STD_IO_SCHEMA_ID,
            STD_UI_SCHEMA_ID,
            STD_JSON_SCHEMA_ID,
            STD_ACTION_SCHEMA_ID,
        ],
        &[
            STD_TERMINAL_DOCUMENT_TYPE_ID,
            STD_IO_BYTE_STREAM_TYPE_ID,
            STD_UI_TYPE_ID,
            STD_JSON_VALUE_TYPE_ID,
            STD_ACTION_TYPE_ID,
        ],
    )
}

/// Reconciles the retained `std/output.orna` unit against the snapshot
/// catalogue and origins.
///
/// The unit must round-trip exactly and contain nothing besides the
/// `std.terminal` and `std.io` schema declarations, the two opaque output
/// value type declarations (`std.terminal.Document` `...15` and
/// `std.io.ByteStream` `...16`, both with their ADR 0058 kernel contracts and
/// their `IMMUTABLE TRANSIENT` catalogue facts), and their two qualified
/// exports (`std.Document`, `std.ByteStream`). Every catalogue definition
/// must sit at the fixed identity and agree with the declaration, and the
/// snapshot origins must cover exactly those six declarations at their exact
/// byte ranges on the retained unit; any extra, missing, or mismatched
/// declaration, identity, contract, binding, or origin fails closed.
fn reconcile_standard_output_unit(
    stored_unit: &StoredSourceUnit,
    parsed_unit: &ParsedSourceUnit,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
) -> Result<(), StandardLibraryCheckError> {
    if parsed_unit.source_text() != stored_unit.content()
        || parsed_unit.source_text() != parsed_unit.syntax_text()
        || !parsed_unit.parsed().object_types().is_empty()
        || !parsed_unit.parsed().enum_types().is_empty()
        || !parsed_unit.parsed().primitive_value_types().is_empty()
        || !parsed_unit.parsed().record_value_types().is_empty()
        || !parsed_unit.parsed().field_renames().is_empty()
        || !parsed_unit.parsed().server_functions().is_empty()
        || !parsed_unit.parsed().client_functions().is_empty()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let [terminal_declaration, io_declaration] = parsed_unit.parsed().schemas() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [document_declaration, bytestream_declaration] = parsed_unit.parsed().opaque_value_types()
    else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [document_export, bytestream_export] = parsed_unit.parsed().type_exports() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };

    let expected_terminal_name = QualifiedSemanticName::new(["std", "terminal"])
        .expect("the fixed standard schema is valid");
    let expected_io_name =
        QualifiedSemanticName::new(["std", "io"]).expect("the fixed standard schema is valid");
    let terminal_name = unquoted_semantic_name(&terminal_declaration.name)?;
    let io_name = unquoted_semantic_name(&io_declaration.name)?;
    if terminal_name != expected_terminal_name || io_name != expected_io_name {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let terminal = catalogue
        .schema_by_id(STD_TERMINAL_SCHEMA_ID)
        .ok_or(StandardLibraryCheckError::MissingSchema)?;
    let io = catalogue
        .schema_by_id(STD_IO_SCHEMA_ID)
        .ok_or(StandardLibraryCheckError::MissingSchema)?;
    if terminal.name() != &terminal_name || io.name() != &io_name {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let expected_document_name = QualifiedSemanticName::new(["std", "terminal", "document"])
        .expect("the fixed standard value type is valid");
    let expected_bytestream_name = QualifiedSemanticName::new(["std", "io", "bytestream"])
        .expect("the fixed standard value type is valid");
    let document_name = unquoted_semantic_name(&document_declaration.name)?;
    let bytestream_name = unquoted_semantic_name(&bytestream_declaration.name)?;
    if document_name != expected_document_name || bytestream_name != expected_bytestream_name {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let document_contract = decode_string_literal(&document_declaration.kernel_contract)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let bytestream_contract = decode_string_literal(&bytestream_declaration.kernel_contract)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let document = catalogue
        .value_type_by_id(STD_TERMINAL_DOCUMENT_TYPE_ID)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let bytestream = catalogue
        .value_type_by_id(STD_IO_BYTE_STREAM_TYPE_ID)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    for (name, contract, definition) in [
        (&document_name, &document_contract, document),
        (&bytestream_name, &bytestream_contract, bytestream),
    ] {
        if definition.name() != name
            || definition.kind() != ValueTypeKind::Opaque
            || definition.mutability() != ValueTypeMutability::Immutable
            || definition.persistence() != ValueTypePersistence::Transient
            || definition.representation_contract() != contract
        {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
    }

    let expected_document_binding_name = QualifiedSemanticName::new(["std", "document"])
        .expect("the fixed standard export is valid");
    let expected_bytestream_binding_name = QualifiedSemanticName::new(["std", "bytestream"])
        .expect("the fixed standard export is valid");
    let document_binding = catalogue
        .type_binding_by_name(&TypeLookupName::qualified(
            expected_document_binding_name.clone(),
        ))
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let bytestream_binding = catalogue
        .type_binding_by_name(&TypeLookupName::qualified(
            expected_bytestream_binding_name.clone(),
        ))
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let document_export_source = unquoted_semantic_name(&document_export.source_type)?;
    let bytestream_export_source = unquoted_semantic_name(&bytestream_export.source_type)?;
    if document_export_source != document_name || bytestream_export_source != bytestream_name {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    for (export, binding, expected_name, expected_target) in [
        (
            document_export,
            document_binding,
            &expected_document_binding_name,
            STD_TERMINAL_DOCUMENT_TYPE_ID,
        ),
        (
            bytestream_export,
            bytestream_binding,
            &expected_bytestream_binding_name,
            STD_IO_BYTE_STREAM_TYPE_ID,
        ),
    ] {
        let TypeExportTarget::Qualified { name } = &export.target else {
            return Err(StandardLibraryCheckError::SourceMismatch);
        };
        if unquoted_semantic_name(name)? != *expected_name
            || !matches!(binding.kind(), TypeBindingKind::Qualified)
            || binding.name() != &TypeLookupName::qualified(expected_name.clone())
            || binding.target() != expected_target
        {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
    }

    let mut origins_by_identity = origin_map(origins)?;
    for (identity, span) in [
        (
            DefinitionIdentity::Schema(STD_TERMINAL_SCHEMA_ID),
            &terminal_declaration.span,
        ),
        (
            DefinitionIdentity::Schema(STD_IO_SCHEMA_ID),
            &io_declaration.span,
        ),
        (
            DefinitionIdentity::ValueType(STD_TERMINAL_DOCUMENT_TYPE_ID),
            &document_declaration.span,
        ),
        (
            DefinitionIdentity::ValueType(STD_IO_BYTE_STREAM_TYPE_ID),
            &bytestream_declaration.span,
        ),
        (
            DefinitionIdentity::TypeBinding(document_binding.id()),
            &document_export.span,
        ),
        (
            DefinitionIdentity::TypeBinding(bytestream_binding.id()),
            &bytestream_export.span,
        ),
    ] {
        take_origin(&mut origins_by_identity, identity, stored_unit.id(), span)?;
    }
    if !origins_by_identity.is_empty() {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    Ok(())
}

/// Reconciles the retained `std/ui.orna` unit against the snapshot catalogue
/// and origins.
///
/// The unit must round-trip exactly and contain nothing besides the
/// `std.ui` schema declaration, the single opaque ui value type declaration
/// (`std.ui.UI` `...19`, with its ADR 0062 kernel contract
/// `orna.std.value.ui@1` and its `IMMUTABLE TRANSIENT` catalogue facts), and
/// the single qualified export (std.UI). Every catalogue definition must sit
/// at the fixed identity and agree with the declaration, and the snapshot
/// origins must cover exactly those three declarations at their exact byte
/// ranges on the retained unit; any extra, missing, or mismatched
/// declaration, identity, contract, binding, or origin fails closed.
fn reconcile_standard_ui_unit(
    stored_unit: &StoredSourceUnit,
    parsed_unit: &ParsedSourceUnit,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
) -> Result<(), StandardLibraryCheckError> {
    if parsed_unit.source_text() != stored_unit.content()
        || parsed_unit.source_text() != parsed_unit.syntax_text()
        || !parsed_unit.parsed().object_types().is_empty()
        || !parsed_unit.parsed().enum_types().is_empty()
        || !parsed_unit.parsed().primitive_value_types().is_empty()
        || !parsed_unit.parsed().record_value_types().is_empty()
        || !parsed_unit.parsed().field_renames().is_empty()
        || !parsed_unit.parsed().server_functions().is_empty()
        || !parsed_unit.parsed().client_functions().is_empty()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let [ui_declaration] = parsed_unit.parsed().schemas() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [ui_type_declaration] = parsed_unit.parsed().opaque_value_types() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [ui_export] = parsed_unit.parsed().type_exports() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };

    let expected_ui_name =
        QualifiedSemanticName::new(["std", "ui"]).expect("the fixed standard schema is valid");
    let ui_name = unquoted_semantic_name(&ui_declaration.name)?;
    if ui_name != expected_ui_name {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let ui_schema = catalogue
        .schema_by_id(STD_UI_SCHEMA_ID)
        .ok_or(StandardLibraryCheckError::MissingSchema)?;
    if ui_schema.name() != &ui_name {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let expected_ui_type_name = QualifiedSemanticName::new(["std", "ui", "ui"])
        .expect("the fixed standard value type is valid");
    let ui_type_name = unquoted_semantic_name(&ui_type_declaration.name)?;
    if ui_type_name != expected_ui_type_name {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let ui_contract = decode_string_literal(&ui_type_declaration.kernel_contract)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    if ui_contract != STD_UI_CONTRACT {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let ui_definition = catalogue
        .value_type_by_id(STD_UI_TYPE_ID)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    if ui_definition.name() != &ui_type_name
        || ui_definition.kind() != ValueTypeKind::Opaque
        || ui_definition.mutability() != ValueTypeMutability::Immutable
        || ui_definition.persistence() != ValueTypePersistence::Transient
        || ui_definition.representation_contract() != ui_contract
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let expected_ui_binding_name =
        QualifiedSemanticName::new(["std", "ui"]).expect("the fixed standard export is valid");
    let ui_binding = catalogue
        .type_binding_by_name(&TypeLookupName::qualified(expected_ui_binding_name.clone()))
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let ui_export_source = unquoted_semantic_name(&ui_export.source_type)?;
    if ui_export_source != ui_type_name {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let TypeExportTarget::Qualified { name } = &ui_export.target else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if unquoted_semantic_name(name)? != expected_ui_binding_name
        || !matches!(ui_binding.kind(), TypeBindingKind::Qualified)
        || ui_binding.name() != &TypeLookupName::qualified(expected_ui_binding_name.clone())
        || ui_binding.target() != STD_UI_TYPE_ID
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let mut origins_by_identity = origin_map(origins)?;
    for (identity, span) in [
        (
            DefinitionIdentity::Schema(STD_UI_SCHEMA_ID),
            &ui_declaration.span,
        ),
        (
            DefinitionIdentity::ValueType(STD_UI_TYPE_ID),
            &ui_type_declaration.span,
        ),
        (
            DefinitionIdentity::TypeBinding(ui_binding.id()),
            &ui_export.span,
        ),
    ] {
        take_origin(&mut origins_by_identity, identity, stored_unit.id(), span)?;
    }
    if !origins_by_identity.is_empty() {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    Ok(())
}
/// Reconciles the retained `std/json.orna` unit against the V5 catalogue and
/// origins. The unit contains the JSON schema, opaque value type, export, and
/// the existing closed `std.json.encode` presenter.
fn reconcile_standard_json_unit(
    stored_unit: &StoredSourceUnit,
    parsed_unit: &ParsedSourceUnit,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
) -> Result<(), StandardLibraryCheckError> {
    if parsed_unit.source_text() != stored_unit.content()
        || parsed_unit.source_text() != parsed_unit.syntax_text()
        || !parsed_unit.parsed().object_types().is_empty()
        || !parsed_unit.parsed().enum_types().is_empty()
        || !parsed_unit.parsed().primitive_value_types().is_empty()
        || !parsed_unit.parsed().record_value_types().is_empty()
        || !parsed_unit.parsed().field_renames().is_empty()
        || !parsed_unit.parsed().client_functions().is_empty()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let [json_declaration] = parsed_unit.parsed().schemas() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [json_type_declaration] = parsed_unit.parsed().opaque_value_types() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [json_export] = parsed_unit.parsed().type_exports() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [json_function] = parsed_unit.parsed().server_functions() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };

    let expected_json_name =
        QualifiedSemanticName::new(["std", "json"]).expect("the fixed standard schema is valid");
    let json_name = unquoted_semantic_name(&json_declaration.name)?;
    if json_name != expected_json_name {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let json_schema = catalogue
        .schema_by_id(STD_JSON_SCHEMA_ID)
        .ok_or(StandardLibraryCheckError::MissingSchema)?;
    if json_schema.name() != &json_name {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let expected_json_type_name = QualifiedSemanticName::new(["std", "json", "value"])
        .expect("the fixed standard value type is valid");
    let json_type_name = unquoted_semantic_name(&json_type_declaration.name)?;
    if json_type_name != expected_json_type_name {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let json_contract = decode_string_literal(&json_type_declaration.kernel_contract)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    if json_contract != STD_JSON_CONTRACT {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let json_definition = catalogue
        .value_type_by_id(STD_JSON_VALUE_TYPE_ID)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    if json_definition.name() != &json_type_name
        || json_definition.kind() != ValueTypeKind::Opaque
        || json_definition.mutability() != ValueTypeMutability::Immutable
        || json_definition.persistence() != ValueTypePersistence::Transient
        || json_definition.representation_contract() != json_contract
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let expected_json_binding_name = QualifiedSemanticName::new(["std", "jsonvalue"])
        .expect("the fixed standard export is valid");
    let json_binding = catalogue
        .type_binding_by_name(&TypeLookupName::qualified(
            expected_json_binding_name.clone(),
        ))
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let json_export_source = unquoted_semantic_name(&json_export.source_type)?;
    let TypeExportTarget::Qualified { name } = &json_export.target else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if json_export_source != json_type_name
        || unquoted_semantic_name(name)? != expected_json_binding_name
        || !matches!(json_binding.kind(), TypeBindingKind::Qualified)
        || json_binding.name() != &TypeLookupName::qualified(expected_json_binding_name.clone())
        || json_binding.target() != STD_JSON_VALUE_TYPE_ID
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    check_standard_json_encode(json_function, catalogue, origins, STD_JSON_VALUE_TYPE_ID)?;

    let mut origins_by_identity = HashMap::with_capacity(origins.len());
    for origin in origins {
        if !matches!(
            origin.identity(),
            DefinitionIdentity::Schema(_)
                | DefinitionIdentity::ValueType(_)
                | DefinitionIdentity::TypeBinding(_)
                | DefinitionIdentity::Function(_)
                | DefinitionIdentity::Parameter { .. }
        ) {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
        if origins_by_identity
            .insert(origin.identity(), origin.source())
            .is_some()
        {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
    }
    for (identity, span) in [
        (
            DefinitionIdentity::Schema(STD_JSON_SCHEMA_ID),
            &json_declaration.span,
        ),
        (
            DefinitionIdentity::ValueType(STD_JSON_VALUE_TYPE_ID),
            &json_type_declaration.span,
        ),
        (
            DefinitionIdentity::TypeBinding(json_binding.id()),
            &json_export.span,
        ),
        (
            DefinitionIdentity::Function(STD_JSON_ENCODE_FUNCTION_ID),
            &json_function.span,
        ),
    ] {
        take_origin(&mut origins_by_identity, identity, stored_unit.id(), span)?;
    }
    let parameter = json_function
        .parameters
        .first()
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    take_origin(
        &mut origins_by_identity,
        DefinitionIdentity::Parameter {
            owner: STD_JSON_ENCODE_FUNCTION_ID,
            parameter: STD_JSON_ENCODE_PARAMETER_ID,
        },
        stored_unit.id(),
        &parameter.span,
    )?;
    if !origins_by_identity.is_empty() {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    Ok(())
}

/// Reconciles the retained `std/action.orna` unit against the V6 catalogue.
fn reconcile_standard_action_unit(
    stored_unit: &StoredSourceUnit,
    parsed_unit: &ParsedSourceUnit,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
) -> Result<StandardSourceFamilies, StandardLibraryCheckError> {
    if parsed_unit.source_text() != stored_unit.content()
        || parsed_unit.source_text() != parsed_unit.syntax_text()
        || !parsed_unit.parsed().object_types().is_empty()
        || !parsed_unit.parsed().enum_types().is_empty()
        || !parsed_unit.parsed().primitive_value_types().is_empty()
        || !parsed_unit.parsed().record_value_types().is_empty()
        || !parsed_unit.parsed().field_renames().is_empty()
        || !parsed_unit.parsed().server_functions().is_empty()
        || !parsed_unit.parsed().client_functions().is_empty()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let [action_schema_declaration] = parsed_unit.parsed().schemas() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [action_type_declaration] = parsed_unit.parsed().opaque_value_types() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [action_export] = parsed_unit.parsed().type_exports() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };

    let expected_schema_name =
        QualifiedSemanticName::new(["std", "action"]).expect("the fixed action schema is valid");
    let schema_name = unquoted_semantic_name(&action_schema_declaration.name)?;
    if schema_name != expected_schema_name
        || catalogue
            .schema_by_id(STD_ACTION_SCHEMA_ID)
            .ok_or(StandardLibraryCheckError::MissingSchema)?
            .name()
            != &schema_name
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let expected_type_name = QualifiedSemanticName::new(["std", "action", "action"])
        .expect("the fixed action value type is valid");
    let type_name = unquoted_semantic_name(&action_type_declaration.name)?;
    let contract = decode_string_literal(&action_type_declaration.kernel_contract)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let action_type = catalogue
        .value_type_by_id(STD_ACTION_TYPE_ID)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    if type_name != expected_type_name
        || action_type.name() != &type_name
        || action_type.kind() != ValueTypeKind::Opaque
        || action_type.mutability() != ValueTypeMutability::Immutable
        || action_type.persistence() != ValueTypePersistence::Transient
        || contract != STD_ACTION_CONTRACT
        || action_type.representation_contract() != contract
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let expected_binding_name =
        QualifiedSemanticName::new(["std", "action"]).expect("the fixed action export is valid");
    let binding = catalogue
        .type_binding_by_name(&TypeLookupName::qualified(expected_binding_name.clone()))
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    let TypeExportTarget::Qualified { name: target_name } = &action_export.target else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    if unquoted_semantic_name(&action_export.source_type)? != type_name
        || unquoted_semantic_name(target_name)? != expected_binding_name
        || !matches!(binding.kind(), TypeBindingKind::Qualified)
        || binding.name() != &TypeLookupName::qualified(expected_binding_name)
        || binding.target() != STD_ACTION_TYPE_ID
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let mut origins_by_identity = origin_map(origins)?;

    let schema_origin = take_origin(
        &mut origins_by_identity,
        DefinitionIdentity::Schema(STD_ACTION_SCHEMA_ID),
        stored_unit.id(),
        &action_schema_declaration.span,
    )?;
    let value_type_origin = take_origin(
        &mut origins_by_identity,
        DefinitionIdentity::ValueType(STD_ACTION_TYPE_ID),
        stored_unit.id(),
        &action_type_declaration.span,
    )?;
    let binding_origin = take_origin(
        &mut origins_by_identity,
        DefinitionIdentity::TypeBinding(binding.id()),
        stored_unit.id(),
        &action_export.span,
    )?;
    if !origins_by_identity.is_empty() {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    Ok(StandardSourceFamilies {
        schemas: vec![CheckedStandardSchema {
            id: STD_ACTION_SCHEMA_ID,
            name: schema_name,
            origin: schema_origin,
        }],
        value_types: vec![CheckedStandardValueType {
            id: STD_ACTION_TYPE_ID,
            name: type_name,
            kind: action_type.kind(),
            mutability: action_type.mutability(),
            persistence: action_type.persistence(),
            representation_contract: action_type.representation_contract().to_owned(),
            origin: value_type_origin,
        }],
        type_bindings: vec![CheckedStandardTypeBinding {
            id: binding.id(),
            kind: binding.kind(),
            name: binding.name().clone(),
            target: binding.target(),
            origin: binding_origin,
        }],
    })
}

/// Splits the snapshot origins into the `std/types.orna` origins (schemas,
/// value types, and bindings) and the `std/invoke.orna` origins (the
/// `std.invoke` schema, the `std.invoke.echo` function, and its parameter).
///
/// Every origin must belong to one of the two retained V2 units; any other
/// source unit fails closed.
fn partition_standard_origins(
    origins: &[DefinitionOrigin],
) -> Result<(Vec<DefinitionOrigin>, Vec<DefinitionOrigin>), StandardLibraryCheckError> {
    let mut types_origins = Vec::new();
    let mut invoke_origins = Vec::new();
    for origin in origins {
        let source_unit = origin.source().source_unit();
        if source_unit == STD_TYPES_SOURCE_UNIT_ID {
            types_origins.push(origin.clone());
        } else if source_unit == STD_INVOKE_SOURCE_UNIT_ID {
            invoke_origins.push(origin.clone());
        } else {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
    }
    Ok((types_origins, invoke_origins))
}

/// The three ordered origin partitions of a V3 standard bundle: the types,
/// invoke, and output unit origins.
type StandardV3OriginPartitions = (
    Vec<DefinitionOrigin>,
    Vec<DefinitionOrigin>,
    Vec<DefinitionOrigin>,
);

/// Splits the snapshot origins into the three retained V3 units: the
/// `std/types.orna` origins, the `std/invoke.orna` origins, and the
/// `std/output.orna` origins (the two output schemas, the two opaque output
/// value types, and their two exports).
///
/// Every origin must belong to one of the three retained V3 units; any other
/// source unit fails closed.
fn partition_standard_v3_origins(
    origins: &[DefinitionOrigin],
) -> Result<StandardV3OriginPartitions, StandardLibraryCheckError> {
    let mut types_origins = Vec::new();
    let mut invoke_origins = Vec::new();
    let mut output_origins = Vec::new();
    for origin in origins {
        let source_unit = origin.source().source_unit();
        if source_unit == STD_TYPES_SOURCE_UNIT_ID {
            types_origins.push(origin.clone());
        } else if source_unit == STD_INVOKE_SOURCE_UNIT_ID {
            invoke_origins.push(origin.clone());
        } else if source_unit == STD_OUTPUT_SOURCE_UNIT_ID {
            output_origins.push(origin.clone());
        } else {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
    }
    Ok((types_origins, invoke_origins, output_origins))
}

/// The four ordered origin partitions of a V4 standard bundle: the types,
/// invoke, output, and ui unit origins.
type StandardV4OriginPartitions = (
    Vec<DefinitionOrigin>,
    Vec<DefinitionOrigin>,
    Vec<DefinitionOrigin>,
    Vec<DefinitionOrigin>,
);

/// Splits the snapshot origins into the four retained V4 units: the
/// `std/types.orna` origins, the `std/invoke.orna` origins, the
/// `std/output.orna` origins, and the `std/ui.orna` origins (the ui schema,
/// the opaque ui value type, and its export).
///
/// Every origin must belong to one of the four retained V4 units; any other
/// source unit fails closed.
fn partition_standard_v4_origins(
    origins: &[DefinitionOrigin],
) -> Result<StandardV4OriginPartitions, StandardLibraryCheckError> {
    let mut types_origins = Vec::new();
    let mut invoke_origins = Vec::new();
    let mut output_origins = Vec::new();
    let mut ui_origins = Vec::new();
    for origin in origins {
        let source_unit = origin.source().source_unit();
        if source_unit == STD_TYPES_SOURCE_UNIT_ID {
            types_origins.push(origin.clone());
        } else if source_unit == STD_INVOKE_SOURCE_UNIT_ID {
            invoke_origins.push(origin.clone());
        } else if source_unit == STD_OUTPUT_SOURCE_UNIT_ID {
            output_origins.push(origin.clone());
        } else if source_unit == STD_UI_SOURCE_UNIT_ID {
            ui_origins.push(origin.clone());
        } else {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
    }
    Ok((types_origins, invoke_origins, output_origins, ui_origins))
}

/// Checks one parsed declaration against the closed ADR 0055 standard
/// parameter-echo source shape.
///
/// The checker accepts ONLY the exact `std.invoke.echo` shape: a SERVER
/// function named `std.invoke.echo` with exactly one required non-null
/// `p_value INTEGER` parameter (no default expression; the grammar has no
/// nullable parameter spelling, so required non-null is the only form), one
/// single `INTEGER` result (never `ROWS`), `SECURITY INVOKER`,
/// `TRANSACTION READ ONLY`, `VOLATILITY STABLE`, zero capability clauses, and
/// the closed no-input `SELECT p_value` body. It rejects every other name,
/// parameter count or name, default, type, result shape, security,
/// transaction, volatility, capability, and body variation before any
/// artifact is constructed.
///
/// The supplied catalogue must contain the fixed identities: the `std.invoke`
/// schema, the `std.invoke.echo` function, and its `p_value` parameter. Both
/// written `INTEGER` spellings must resolve through the catalogue to
/// `integer_type_id`, which therefore must hold a value type at that identity.
/// The supplied origins must contain the fixed function and parameter
/// declaration origins; the reference source origins reuse the retained
/// source unit from the function origin and the exact byte ranges of the
/// `INTEGER`, `INTEGER`, and `p_value` tokens in the declaration.
///
/// Step 6 (`feat(compiler): reconcile executable standard source`) wires this
/// checker into the standard source checker and consumes the returned facts
/// to build the `StandardExecutable` record: the fixed function identity, the
/// version-1 revision identity, the 44-byte `orna.server-parameter-echo`
/// artifact, and the three ordered durable references.
pub fn check_standard_parameter_echo(
    declaration: &ServerFunctionDeclaration,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    integer_type_id: TypeId,
) -> Result<CheckedStandardParameterEcho, StandardLibraryCheckError> {
    let expected_name = QualifiedSemanticName::new(["std", "invoke", "echo"])
        .expect("the fixed standard function name is valid");
    let name = semantic_name(&declaration.name);
    if name != expected_name {
        return Err(StandardLibraryCheckError::UnexpectedName { actual: name });
    }

    if declaration.parameters.len() != 1 {
        return Err(StandardLibraryCheckError::UnexpectedParameterCount {
            actual: declaration.parameters.len(),
        });
    }
    let parameter = &declaration.parameters[0];
    let parameter_name = semantic_part(&parameter.name);
    if parameter_name != "p_value" {
        return Err(StandardLibraryCheckError::UnexpectedParameterName {
            actual: parameter_name,
        });
    }
    if parameter.default_expression.is_some() {
        return Err(StandardLibraryCheckError::ParameterDefault);
    }
    if resolved_standard_type_id(&parameter.type_specification, catalogue) != Some(integer_type_id)
    {
        return Err(StandardLibraryCheckError::UnexpectedParameterType);
    }
    let parameter_type_span = parameter.type_specification.span();

    let FunctionReturnType::Single(result_specification) = &declaration.return_type else {
        return Err(StandardLibraryCheckError::UnexpectedResultShape);
    };
    if resolved_standard_type_id(result_specification, catalogue) != Some(integer_type_id) {
        return Err(StandardLibraryCheckError::UnexpectedResultType);
    }
    let result_type_span = result_specification.span();

    let security = declaration
        .security
        .ok_or(StandardLibraryCheckError::MissingSecurity)?;
    if security != SyntaxFunctionSecurity::Invoker {
        return Err(StandardLibraryCheckError::UnexpectedSecurity { actual: security });
    }
    let transaction = declaration
        .transaction
        .ok_or(StandardLibraryCheckError::MissingTransaction)?;
    if transaction != SyntaxFunctionTransaction::ReadOnly {
        return Err(StandardLibraryCheckError::UnexpectedTransaction {
            actual: transaction,
        });
    }
    let volatility = declaration
        .volatility
        .ok_or(StandardLibraryCheckError::MissingVolatility)?;
    if volatility != SyntaxFunctionVolatility::Stable {
        return Err(StandardLibraryCheckError::UnexpectedVolatility { actual: volatility });
    }
    if !declaration.capabilities.is_empty() {
        return Err(StandardLibraryCheckError::CapabilityClause);
    }

    let body = declaration
        .body
        .as_no_input_parameter_select()
        .ok_or(StandardLibraryCheckError::UnexpectedBody)?;
    let body_identifier = semantic_part(&body.parameter);
    if body_identifier != "p_value" {
        return Err(StandardLibraryCheckError::UnexpectedBodyIdentifier {
            actual: body_identifier,
        });
    }
    let body_identifier_span = &body.parameter.span;

    let expected_schema_name =
        QualifiedSemanticName::new(["std", "invoke"]).expect("the fixed standard schema is valid");
    let schema = catalogue
        .schema_by_id(STD_INVOKE_SCHEMA_ID)
        .ok_or(StandardLibraryCheckError::MissingSchema)?;
    if schema.name() != &expected_schema_name {
        return Err(StandardLibraryCheckError::SchemaNameMismatch {
            actual: schema.name().clone(),
        });
    }
    let function = catalogue
        .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
        .ok_or(StandardLibraryCheckError::MissingFunction)?;
    if function.name() != &expected_name {
        return Err(StandardLibraryCheckError::FunctionNameMismatch {
            actual: function.name().clone(),
        });
    }
    let parameter_definition = function
        .parameter_by_id(STD_INVOKE_ECHO_PARAMETER_ID)
        .ok_or(StandardLibraryCheckError::MissingParameter)?;
    if parameter_definition.name() != "p_value" {
        return Err(StandardLibraryCheckError::ParameterNameMismatch {
            actual: parameter_definition.name().to_owned(),
        });
    }

    let function_origin = origins
        .iter()
        .find(|origin| {
            origin.identity() == DefinitionIdentity::Function(STD_INVOKE_ECHO_FUNCTION_ID)
        })
        .ok_or(StandardLibraryCheckError::MissingFunctionOrigin)?;
    let parameter_origin = origins
        .iter()
        .find(|origin| {
            origin.identity()
                == DefinitionIdentity::Parameter {
                    owner: STD_INVOKE_ECHO_FUNCTION_ID,
                    parameter: STD_INVOKE_ECHO_PARAMETER_ID,
                }
        })
        .ok_or(StandardLibraryCheckError::MissingParameterOrigin)?;
    if function_origin.source().source_unit() != parameter_origin.source().source_unit() {
        return Err(StandardLibraryCheckError::OriginSourceUnitMismatch);
    }
    let source_unit = function_origin.source().source_unit();

    let payload = ServerParameterEcho::new(STD_INVOKE_ECHO_PARAMETER_ID, integer_type_id)
        .map_err(|source| StandardLibraryCheckError::Artifact { source })?
        .encode()
        .map_err(|source| StandardLibraryCheckError::Artifact { source })?;
    let content_hash = artifact_payload_digest(&payload)
        .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        server_parameter_echo::FORMAT_IDENTITY,
        server_parameter_echo::FORMAT_VERSION,
        payload,
        content_hash,
    )
    .map_err(|source| StandardLibraryCheckError::Revision { source })?;

    let reference_origin = |span: &SourceSpan| -> Result<SourceOrigin, StandardLibraryCheckError> {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
        let end = u32::try_from(span.end).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
        SourceOrigin::new(source_unit, start, end)
            .map_err(|source| StandardLibraryCheckError::Revision { source })
    };
    let references = vec![
        DefinitionReference::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            0,
            DefinitionReferenceTarget::ValueType(integer_type_id),
            DefinitionReferenceKind::NamedType,
            reference_origin(parameter_type_span)?,
        ),
        DefinitionReference::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            1,
            DefinitionReferenceTarget::ValueType(integer_type_id),
            DefinitionReferenceKind::NamedType,
            reference_origin(result_type_span)?,
        ),
        DefinitionReference::new(
            STD_INVOKE_ECHO_FUNCTION_ID,
            STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
            2,
            DefinitionReferenceTarget::Parameter {
                owner: STD_INVOKE_ECHO_FUNCTION_ID,
                parameter: STD_INVOKE_ECHO_PARAMETER_ID,
            },
            DefinitionReferenceKind::ParameterRead,
            reference_origin(body_identifier_span)?,
        ),
    ];

    Ok(CheckedStandardParameterEcho {
        function_id: STD_INVOKE_ECHO_FUNCTION_ID,
        parameter_id: STD_INVOKE_ECHO_PARAMETER_ID,
        revision_id: STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        artifact,
        references,
    })
}

fn reconcile_standard_invoke_executable(
    catalogue: &CatalogueSnapshot,
    invoke_origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
    stored_unit: &StoredSourceUnit,
    parsed_unit: &ParsedSourceUnit,
) -> Result<CheckedStandardExecutable, StandardLibraryCheckError> {
    if parsed_unit.source_text() != stored_unit.content()
        || parsed_unit.source_text() != parsed_unit.syntax_text()
        || !parsed_unit.parsed().object_types().is_empty()
        || !parsed_unit.parsed().enum_types().is_empty()
        || !parsed_unit.parsed().primitive_value_types().is_empty()
        || !parsed_unit.parsed().opaque_value_types().is_empty()
        || !parsed_unit.parsed().record_value_types().is_empty()
        || !parsed_unit.parsed().field_renames().is_empty()
        || !parsed_unit.parsed().type_exports().is_empty()
        || !parsed_unit.parsed().client_functions().is_empty()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    let [schema_declaration] = parsed_unit.parsed().schemas() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let [function_declaration] = parsed_unit.parsed().server_functions() else {
        return Err(StandardLibraryCheckError::SourceMismatch);
    };
    let expected_schema_name =
        QualifiedSemanticName::new(["std", "invoke"]).expect("the fixed standard schema is valid");
    let schema_name = unquoted_semantic_name(&schema_declaration.name)?;
    if schema_name != expected_schema_name {
        return Err(StandardLibraryCheckError::SchemaNameMismatch {
            actual: schema_name,
        });
    }

    let checked = check_standard_parameter_echo(
        function_declaration,
        catalogue,
        invoke_origins,
        STD_INTEGER_TYPE_ID,
    )?;

    let source_origin = |span: &SourceSpan| -> Result<SourceOrigin, StandardLibraryCheckError> {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
        let end = u32::try_from(span.end).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
        SourceOrigin::new(stored_unit.id(), start, end)
            .map_err(|_| StandardLibraryCheckError::SourceMismatch)
    };
    let expected_schema_origin = source_origin(&schema_declaration.span)?;
    let expected_function_origin = source_origin(&function_declaration.span)?;
    let expected_parameter_origin = source_origin(&function_declaration.parameters[0].span)?;

    let schema_origin = expect_invoke_origin(
        invoke_origins,
        DefinitionIdentity::Schema(STD_INVOKE_SCHEMA_ID),
        expected_schema_origin,
        StandardLibraryCheckError::MissingSchemaOrigin,
    )?;
    let function_origin = expect_invoke_origin(
        invoke_origins,
        DefinitionIdentity::Function(STD_INVOKE_ECHO_FUNCTION_ID),
        expected_function_origin,
        StandardLibraryCheckError::MissingFunctionOrigin,
    )?;
    let parameter_origin = expect_invoke_origin(
        invoke_origins,
        DefinitionIdentity::Parameter {
            owner: STD_INVOKE_ECHO_FUNCTION_ID,
            parameter: STD_INVOKE_ECHO_PARAMETER_ID,
        },
        expected_parameter_origin,
        StandardLibraryCheckError::MissingParameterOrigin,
    )?;
    if invoke_origins.len() != 3 {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let declaration_bytes = &stored_unit.content().as_bytes()[expected_function_origin.byte_start()
        as usize
        ..expected_function_origin.byte_end() as usize];
    let declaration_content_hash = function_declaration_digest(declaration_bytes)
        .map_err(|source| StandardLibraryCheckError::Digest { source })?;
    let function = catalogue
        .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
        .ok_or(StandardLibraryCheckError::MissingFunction)?;
    let semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        server_parameter_echo::LANGUAGE_VERSION_IDENTITY,
        checked.artifact(),
        &[],
        checked.references(),
    )
    .map_err(|source| StandardLibraryCheckError::Digest { source })?;

    let checked_executable = CheckedStandardExecutable {
        function_id: checked.function_id(),
        parameter_ids: vec![checked.parameter_id()],
        revision_id: checked.revision_id(),
        revision_number: STD_INVOKE_ECHO_REVISION_NUMBER,
        declaration_origin: expected_function_origin,
        declaration_content_hash,
        semantic_hash,
        semantic_hash_version: FunctionSemanticHashVersion::Version2,
        language_version: server_parameter_echo::LANGUAGE_VERSION_IDENTITY.to_owned(),
        artifact: checked.artifact().clone(),
        references: checked.references().to_vec(),
        schema_origin,
        function_origin,
        parameter_origins: vec![parameter_origin],
    };

    let [stored_executable] = executables else {
        return Err(StandardLibraryCheckError::ExecutableCount {
            actual: executables.len(),
        });
    };
    reconcile_standard_executable(stored_executable, &checked_executable)?;
    Ok(checked_executable)
}

/// Cross-checks one stored standard executable against the checked source
/// facts. Every stored fact must agree exactly, or the snapshot fails closed.
pub(super) fn reconcile_standard_executable(
    stored: &StandardExecutable,
    checked: &CheckedStandardExecutable,
) -> Result<(), StandardLibraryCheckError> {
    if stored.function() != checked.function_id()
        || stored.revision().id() != checked.revision_id()
        || stored.revision().revision_number() != checked.revision_number()
        || stored.revision().semantic_hash_version() != checked.semantic_hash_version()
        || stored.revision().language_version() != checked.language_version()
        || stored.revision().declaration_origin() != checked.declaration_origin()
        || stored.revision().declaration_content_hash() != checked.declaration_content_hash()
        || stored.revision().semantic_hash() != checked.semantic_hash()
        || stored.revision().artifact() != checked.artifact()
        || stored.references() != checked.references()
    {
        return Err(StandardLibraryCheckError::ExecutableMismatch);
    }
    Ok(())
}

/// Requires exactly one origin with the fixed identity and the exact expected
/// range. A missing, duplicated, or range-mismatched origin fails closed.
fn expect_invoke_origin(
    origins: &[DefinitionOrigin],
    identity: DefinitionIdentity,
    expected: SourceOrigin,
    missing: StandardLibraryCheckError,
) -> Result<SourceOrigin, StandardLibraryCheckError> {
    let mut matches = 0;
    for origin in origins {
        if origin.identity() == identity {
            matches += 1;
            if origin.source() != expected {
                return Err(StandardLibraryCheckError::SourceMismatch);
            }
        }
    }
    if matches == 1 {
        Ok(expected)
    } else {
        Err(missing)
    }
}

/// Resolves one written type specification to its durable type identity in
/// the supplied catalogue, mirroring the standard prelude and qualified
/// lookup rules used by application type resolution.
fn resolved_standard_type_id(
    specification: &TypeSpecification,
    catalogue: &CatalogueSnapshot,
) -> Option<TypeId> {
    let TypeSpecification::Named(name) = specification else {
        return None;
    };
    if name.parts.len() == 1 && !name.parts[0].text.starts_with('"') {
        let prelude = PreludeTypeName::new([semantic_part(&name.parts[0])]).ok()?;
        catalogue.type_id_by_name(&TypeLookupName::prelude(prelude))
    } else {
        catalogue.type_id_by_name(&TypeLookupName::qualified(semantic_name(name)))
    }
}

#[cfg(test)]
pub(crate) fn checked_standard_library_with_contract_overrides_for_test(
    snapshot: &VerifiedStandardLibrarySnapshot,
    overrides: &[(usize, &str)],
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    let mut checked = check_standard_library_source(snapshot)?;
    for (index, contract) in overrides {
        let Some(value_type) = checked.value_types.get_mut(*index) else {
            return Err(StandardLibraryCheckError::SourceMismatch);
        };
        value_type.representation_contract = (*contract).to_owned();
    }
    Ok(checked)
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct StandardSourceFamilies {
    pub(super) schemas: Vec<CheckedStandardSchema>,
    pub(super) value_types: Vec<CheckedStandardValueType>,
    pub(super) type_bindings: Vec<CheckedStandardTypeBinding>,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct PendingStandardSourceFacts {
    schemas: Vec<PendingStandardSchema>,
    value_types: Vec<PendingStandardValueType>,
    type_bindings: Vec<PendingStandardTypeBinding>,
}

#[derive(Debug, Eq, PartialEq)]
struct PendingStandardSchema {
    id: orna_core::SchemaId,
    name: QualifiedSemanticName,
    span: SourceSpan,
}

#[derive(Debug, Eq, PartialEq)]
struct PendingStandardValueType {
    id: orna_core::TypeId,
    name: QualifiedSemanticName,
    kind: ValueTypeKind,
    mutability: ValueTypeMutability,
    persistence: ValueTypePersistence,
    representation_contract: String,
    span: SourceSpan,
}

#[derive(Debug, Eq, PartialEq)]
struct PendingStandardTypeBinding {
    id: orna_core::TypeBindingId,
    kind: TypeBindingKind,
    name: TypeLookupName,
    target: orna_core::TypeId,
    span: SourceSpan,
}

pub(super) fn reconcile_standard_source(
    stored_unit: &StoredSourceUnit,
    parsed_unit: &ParsedSourceUnit,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
) -> Result<StandardSourceFamilies, StandardLibraryCheckError> {
    validate_standard_source_shape(stored_unit, parsed_unit, catalogue)?;
    let pending = match_standard_source_facts(parsed_unit, catalogue)?;
    validate_standard_source_origins(stored_unit, origins, pending)
}

fn validate_standard_source_shape(
    stored_unit: &StoredSourceUnit,
    parsed_unit: &ParsedSourceUnit,
    catalogue: &CatalogueSnapshot,
) -> Result<(), StandardLibraryCheckError> {
    if parsed_unit.source_text() != stored_unit.content()
        || parsed_unit.source_text() != parsed_unit.syntax_text()
        || !catalogue.object_types().is_empty()
        || !catalogue.enum_types().is_empty()
        || !catalogue.record_value_types().is_empty()
        || !catalogue.functions().is_empty()
        || !parsed_unit.parsed().object_types().is_empty()
        || !parsed_unit.parsed().enum_types().is_empty()
        || !parsed_unit.parsed().record_value_types().is_empty()
        || !parsed_unit.parsed().field_renames().is_empty()
        || !parsed_unit.parsed().server_functions().is_empty()
        || !parsed_unit.parsed().client_functions().is_empty()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    let (qualified_binding_count, prelude_binding_count) =
        catalogue_binding_category_counts(catalogue)?;
    let (qualified_export_count, prelude_export_count) =
        source_export_category_counts(parsed_unit)?;
    if parsed_unit.parsed().schemas().len() != catalogue.schemas().len()
        || parsed_unit.parsed().primitive_value_types().len()
            + parsed_unit.parsed().opaque_value_types().len()
            != catalogue.value_types().len()
        || qualified_export_count != qualified_binding_count
        || prelude_export_count != prelude_binding_count
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    Ok(())
}

pub(super) fn match_standard_source_facts(
    parsed_unit: &ParsedSourceUnit,
    catalogue: &CatalogueSnapshot,
) -> Result<PendingStandardSourceFacts, StandardLibraryCheckError> {
    let mut consumed_schema_ids = HashSet::with_capacity(catalogue.schemas().len());
    let mut consumed_type_ids = HashSet::with_capacity(catalogue.value_types().len());
    let mut consumed_binding_ids = HashSet::with_capacity(catalogue.type_bindings().len());

    let mut schemas = Vec::with_capacity(parsed_unit.parsed().schemas().len());
    for declaration in parsed_unit.parsed().schemas() {
        let name = unquoted_semantic_name(&declaration.name)?;
        let definition = catalogue
            .schema_by_name(&name)
            .ok_or(StandardLibraryCheckError::SourceMismatch)?;
        if !consumed_schema_ids.insert(definition.id()) {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
        schemas.push(PendingStandardSchema {
            id: definition.id(),
            name,
            span: declaration.span.clone(),
        });
    }

    let mut primary_type_ids = HashMap::with_capacity(catalogue.value_types().len());
    let mut value_types = Vec::with_capacity(catalogue.value_types().len());
    let mut match_value_type = |name: QualifiedSemanticName,
                                kind: ValueTypeKind,
                                persistence: ValueTypePersistence,
                                contract: String,
                                span: SourceSpan|
     -> Result<PendingStandardValueType, StandardLibraryCheckError> {
        let definition = catalogue
            .value_type_by_name(&name)
            .ok_or(StandardLibraryCheckError::SourceMismatch)?;
        if definition.kind() != kind
            || definition.mutability() != ValueTypeMutability::Immutable
            || definition.persistence() != persistence
            || definition.representation_contract() != contract
            || !consumed_type_ids.insert(definition.id())
            || primary_type_ids
                .insert(name.clone(), definition.id())
                .is_some()
        {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
        Ok(PendingStandardValueType {
            id: definition.id(),
            name,
            kind: definition.kind(),
            mutability: definition.mutability(),
            persistence: definition.persistence(),
            representation_contract: definition.representation_contract().to_owned(),
            span,
        })
    };
    for declaration in parsed_unit.parsed().primitive_value_types() {
        let name = unquoted_semantic_name(&declaration.name)?;
        let contract = decode_string_literal(&declaration.kernel_contract)
            .ok_or(StandardLibraryCheckError::SourceMismatch)?;
        let persistence = value_type_persistence(declaration.persistence);
        value_types.push(match_value_type(
            name,
            ValueTypeKind::Primitive,
            persistence,
            contract,
            declaration.span.clone(),
        )?);
    }
    for declaration in parsed_unit.parsed().opaque_value_types() {
        let name = unquoted_semantic_name(&declaration.name)?;
        let contract = decode_string_literal(&declaration.kernel_contract)
            .filter(|contract| opaque_contract_is_valid(contract))
            .ok_or(StandardLibraryCheckError::SourceMismatch)?;
        value_types.push(match_value_type(
            name,
            ValueTypeKind::Opaque,
            ValueTypePersistence::Transient,
            contract,
            declaration.span.clone(),
        )?);
    }
    value_types.sort_by_key(|value_type| value_type.span.start);

    let type_exports = parsed_unit.parsed().type_exports();
    let mut qualified_bindings = (0..type_exports.len()).map(|_| None).collect::<Vec<_>>();
    let mut qualified_targets = HashMap::with_capacity(catalogue.type_bindings().len());
    for (index, declaration) in type_exports.iter().enumerate() {
        let TypeExportTarget::Qualified { name } = &declaration.target else {
            continue;
        };
        let source_name = unquoted_semantic_name(&declaration.source_type)?;
        let target_name = unquoted_semantic_name(name)?;
        let target = primary_type_ids
            .get(&source_name)
            .copied()
            .ok_or(StandardLibraryCheckError::SourceMismatch)?;
        let lookup_name = TypeLookupName::qualified(target_name.clone());
        let binding = catalogue
            .type_binding_by_name(&lookup_name)
            .ok_or(StandardLibraryCheckError::SourceMismatch)?;
        if !matches!(binding.kind(), TypeBindingKind::Qualified)
            || binding.target() != target
            || !consumed_binding_ids.insert(binding.id())
            || qualified_targets.insert(target_name, target).is_some()
        {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
        qualified_bindings[index] = Some(PendingStandardTypeBinding {
            id: binding.id(),
            kind: binding.kind(),
            name: binding.name().clone(),
            target: binding.target(),
            span: declaration.span.clone(),
        });
    }

    let mut type_bindings = Vec::with_capacity(type_exports.len());
    for (index, declaration) in type_exports.iter().enumerate() {
        match &declaration.target {
            TypeExportTarget::Qualified { .. } => {
                let binding = qualified_bindings[index]
                    .take()
                    .ok_or(StandardLibraryCheckError::SourceMismatch)?;
                type_bindings.push(binding);
            }
            TypeExportTarget::Prelude { words, .. } => {
                let source_name = unquoted_semantic_name(&declaration.source_type)?;
                let target = qualified_targets
                    .get(&source_name)
                    .copied()
                    .ok_or(StandardLibraryCheckError::SourceMismatch)?;
                let prelude_name = unquoted_prelude_name(words)?;
                let lookup_name = TypeLookupName::prelude(prelude_name);
                let binding = catalogue
                    .type_binding_by_name(&lookup_name)
                    .ok_or(StandardLibraryCheckError::SourceMismatch)?;
                if !matches!(binding.kind(), TypeBindingKind::Prelude)
                    || binding.target() != target
                    || !consumed_binding_ids.insert(binding.id())
                {
                    return Err(StandardLibraryCheckError::SourceMismatch);
                }
                type_bindings.push(PendingStandardTypeBinding {
                    id: binding.id(),
                    kind: binding.kind(),
                    name: binding.name().clone(),
                    target: binding.target(),
                    span: declaration.span.clone(),
                });
            }
        }
    }

    if consumed_schema_ids.len() != catalogue.schemas().len()
        || consumed_type_ids.len() != catalogue.value_types().len()
        || consumed_binding_ids.len() != catalogue.type_bindings().len()
    {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    Ok(PendingStandardSourceFacts {
        schemas,
        value_types,
        type_bindings,
    })
}

pub(super) fn validate_standard_source_origins(
    stored_unit: &StoredSourceUnit,
    origins: &[DefinitionOrigin],
    pending: PendingStandardSourceFacts,
) -> Result<StandardSourceFamilies, StandardLibraryCheckError> {
    let mut origins_by_identity = origin_map(origins)?;
    let schemas = pending
        .schemas
        .into_iter()
        .map(|fact| {
            let origin = take_origin(
                &mut origins_by_identity,
                DefinitionIdentity::Schema(fact.id),
                stored_unit.id(),
                &fact.span,
            )?;
            Ok(CheckedStandardSchema {
                id: fact.id,
                name: fact.name,
                origin,
            })
        })
        .collect::<Result<Vec<_>, StandardLibraryCheckError>>()?;
    let value_types = pending
        .value_types
        .into_iter()
        .map(|fact| {
            let origin = take_origin(
                &mut origins_by_identity,
                DefinitionIdentity::ValueType(fact.id),
                stored_unit.id(),
                &fact.span,
            )?;
            Ok(CheckedStandardValueType {
                id: fact.id,
                name: fact.name,
                kind: fact.kind,
                mutability: fact.mutability,
                persistence: fact.persistence,
                representation_contract: fact.representation_contract,
                origin,
            })
        })
        .collect::<Result<Vec<_>, StandardLibraryCheckError>>()?;
    let type_bindings = pending
        .type_bindings
        .into_iter()
        .map(|fact| {
            let origin = take_origin(
                &mut origins_by_identity,
                DefinitionIdentity::TypeBinding(fact.id),
                stored_unit.id(),
                &fact.span,
            )?;
            Ok(CheckedStandardTypeBinding {
                id: fact.id,
                kind: fact.kind,
                name: fact.name,
                target: fact.target,
                origin,
            })
        })
        .collect::<Result<Vec<_>, StandardLibraryCheckError>>()?;
    if !origins_by_identity.is_empty() {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }

    Ok(StandardSourceFamilies {
        schemas,
        value_types,
        type_bindings,
    })
}

fn catalogue_binding_category_counts(
    catalogue: &CatalogueSnapshot,
) -> Result<(usize, usize), StandardLibraryCheckError> {
    let mut qualified = 0;
    let mut prelude = 0;
    for binding in catalogue.type_bindings() {
        match binding.kind() {
            TypeBindingKind::Qualified => qualified += 1,
            TypeBindingKind::Prelude => prelude += 1,
            _ => return Err(StandardLibraryCheckError::SourceMismatch),
        }
    }
    Ok((qualified, prelude))
}

fn source_export_category_counts(
    parsed_unit: &ParsedSourceUnit,
) -> Result<(usize, usize), StandardLibraryCheckError> {
    let mut qualified = 0;
    let mut prelude = 0;
    for declaration in parsed_unit.parsed().type_exports() {
        match &declaration.target {
            TypeExportTarget::Qualified { .. } => qualified += 1,
            TypeExportTarget::Prelude { .. } => prelude += 1,
        }
    }
    Ok((qualified, prelude))
}

fn origin_map(
    origins: &[DefinitionOrigin],
) -> Result<HashMap<DefinitionIdentity, SourceOrigin>, StandardLibraryCheckError> {
    let mut by_identity = HashMap::with_capacity(origins.len());
    for origin in origins {
        match origin.identity() {
            DefinitionIdentity::Schema(_)
            | DefinitionIdentity::ValueType(_)
            | DefinitionIdentity::TypeBinding(_) => {}
            _ => return Err(StandardLibraryCheckError::SourceMismatch),
        }
        if by_identity
            .insert(origin.identity(), origin.source())
            .is_some()
        {
            return Err(StandardLibraryCheckError::SourceMismatch);
        }
    }
    Ok(by_identity)
}

fn take_origin(
    origins: &mut HashMap<DefinitionIdentity, SourceOrigin>,
    identity: DefinitionIdentity,
    source_unit: orna_core::SourceUnitId,
    span: &SourceSpan,
) -> Result<SourceOrigin, StandardLibraryCheckError> {
    let byte_start =
        u32::try_from(span.start).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let byte_end =
        u32::try_from(span.end).map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let expected = SourceOrigin::new(source_unit, byte_start, byte_end)
        .map_err(|_| StandardLibraryCheckError::SourceMismatch)?;
    let actual = origins
        .remove(&identity)
        .ok_or(StandardLibraryCheckError::SourceMismatch)?;
    if actual != expected {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    Ok(actual)
}

pub(super) fn unquoted_semantic_name(
    name: &QualifiedName,
) -> Result<QualifiedSemanticName, StandardLibraryCheckError> {
    if name.parts.iter().any(|part| part.text.starts_with('"')) {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    QualifiedSemanticName::new(name.parts.iter().map(semantic_part))
        .map_err(|_| StandardLibraryCheckError::SourceMismatch)
}
fn matches_qualified_name(name: &QualifiedName, expected: &QualifiedSemanticName) -> bool {
    unquoted_semantic_name(name)
        .ok()
        .is_some_and(|actual| actual == *expected)
}

pub(super) fn unquoted_prelude_name(
    words: &[orna_syntax::NamePart],
) -> Result<PreludeTypeName, StandardLibraryCheckError> {
    if words.iter().any(|word| word.text.starts_with('"')) {
        return Err(StandardLibraryCheckError::SourceMismatch);
    }
    PreludeTypeName::new(words.iter().map(semantic_part))
        .map_err(|_| StandardLibraryCheckError::SourceMismatch)
}

fn value_type_persistence(persistence: PrimitiveValueTypePersistence) -> ValueTypePersistence {
    match persistence {
        PrimitiveValueTypePersistence::Persistable => ValueTypePersistence::Persistable,
        PrimitiveValueTypePersistence::Transient => ValueTypePersistence::Transient,
    }
}
