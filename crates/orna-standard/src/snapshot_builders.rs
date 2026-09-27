use super::*;

pub(super) fn retained_standard_library_v4_snapshot_from_source(
    types_source: &str,
    invoke_source: &str,
    output_source: &str,
    ui_source: &str,
) -> Result<StandardLibrarySnapshot, StandardLibraryError> {
    let manifest = standard_library_v4_manifest()
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
    let ui_origins = reconcile_retained_ui_source(ui_source, catalogue)?;
    origins.extend(ui_origins.iter().cloned());

    let types_content_hash = source_unit_content_digest(types_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if types_content_hash != ACCEPTED_V4_TYPES_CONTENT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let invoke_content_hash = source_unit_content_digest(invoke_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if invoke_content_hash != ACCEPTED_V4_INVOKE_CONTENT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let output_content_hash = source_unit_content_digest(output_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if output_content_hash != ACCEPTED_V4_OUTPUT_CONTENT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let ui_content_hash = source_unit_content_digest(ui_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if ui_content_hash != ACCEPTED_V4_UI_CONTENT_DIGEST {
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
    let ui_unit = StoredSourceUnit::new(
        STD_UI_SOURCE_UNIT_ID,
        3,
        STD_UI_SOURCE_LOGICAL_PATH,
        ui_source,
        ui_content_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let units = vec![types_unit, invoke_unit, output_unit, ui_unit];
    let bundle_hash = source_bundle_digest(&units)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if bundle_hash != ACCEPTED_V4_SOURCE_BUNDLE_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let revision_hash = source_revision_record_digest(
        STANDARD_SOURCE_V4_BUNDLE_ID,
        Some(STANDARD_SOURCE_V3_REVISION_ID),
        bundle_hash,
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if revision_hash != ACCEPTED_V4_SOURCE_REVISION_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let retained_source = StoredSourceRevision::new(
        STANDARD_SOURCE_V4_BUNDLE_ID,
        STANDARD_SOURCE_V4_REVISION_ID,
        Some(STANDARD_SOURCE_V3_REVISION_ID),
        units,
        bundle_hash,
        revision_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;

    // `orna.std/4` retains the exact V2 parameter-echo executable unchanged;
    // its artifact and semantic digests are the V3 goldens, pinned here as the
    // V4 goldens so the retained path fails closed on any drift.
    let executable = retained_v2_executable(invoke_source, catalogue, &invoke_origins)?;
    if executable.revision().artifact().content_hash() != ACCEPTED_V4_ARTIFACT_DIGEST
        || executable.revision().semantic_hash() != ACCEPTED_V4_SEMANTIC_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let snapshot = StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V4_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        retained_source,
        LANGUAGE_VERSION_IDENTITY,
        catalogue.clone(),
        vec![executable],
        origins,
        ACCEPTED_V4_STANDARD_LIBRARY_DIGEST,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let _ = standard_library_digest(&snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;

    Ok(snapshot)
}

pub(super) fn retained_standard_library_v5_snapshot_from_source(
    types_source: &str,
    invoke_source: &str,
    output_source: &str,
    ui_source: &str,
    json_source: &str,
) -> Result<StandardLibrarySnapshot, StandardLibraryError> {
    let manifest = standard_library_v5_manifest()
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
    origins.extend(reconcile_retained_output_source(output_source, catalogue)?);
    origins.extend(reconcile_retained_ui_source(ui_source, catalogue)?);
    let json_origins = reconcile_retained_json_source(json_source, catalogue)?;
    origins.extend(json_origins.iter().cloned());

    let types_content_hash = source_unit_content_digest(types_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let invoke_content_hash = source_unit_content_digest(invoke_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let output_content_hash = source_unit_content_digest(output_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let ui_content_hash = source_unit_content_digest(ui_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let json_content_hash = source_unit_content_digest(json_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if types_content_hash != ACCEPTED_V5_TYPES_CONTENT_DIGEST
        || invoke_content_hash != ACCEPTED_V5_INVOKE_CONTENT_DIGEST
        || output_content_hash != ACCEPTED_V5_OUTPUT_CONTENT_DIGEST
        || ui_content_hash != ACCEPTED_V5_UI_CONTENT_DIGEST
        || json_content_hash != ACCEPTED_V5_JSON_CONTENT_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let units = vec![
        StoredSourceUnit::new(
            STD_TYPES_SOURCE_UNIT_ID,
            0,
            SOURCE_LOGICAL_PATH,
            types_source,
            types_content_hash,
        ),
        StoredSourceUnit::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            1,
            STD_INVOKE_SOURCE_LOGICAL_PATH,
            invoke_source,
            invoke_content_hash,
        ),
        StoredSourceUnit::new(
            STD_OUTPUT_SOURCE_UNIT_ID,
            2,
            STD_OUTPUT_SOURCE_LOGICAL_PATH,
            output_source,
            output_content_hash,
        ),
        StoredSourceUnit::new(
            STD_UI_SOURCE_UNIT_ID,
            3,
            STD_UI_SOURCE_LOGICAL_PATH,
            ui_source,
            ui_content_hash,
        ),
        StoredSourceUnit::new(
            STD_JSON_SOURCE_UNIT_ID,
            4,
            STD_JSON_SOURCE_LOGICAL_PATH,
            json_source,
            json_content_hash,
        ),
    ]
    .into_iter()
    .map(|unit| unit.map_err(|source| StandardLibraryError::Revision { source }))
    .collect::<Result<Vec<_>, _>>()?;
    let bundle_hash = source_bundle_digest(&units)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if bundle_hash != ACCEPTED_V5_SOURCE_BUNDLE_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let revision_hash = source_revision_record_digest(
        STANDARD_SOURCE_V5_BUNDLE_ID,
        Some(STANDARD_SOURCE_V4_REVISION_ID),
        bundle_hash,
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if revision_hash != ACCEPTED_V5_SOURCE_REVISION_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let retained_source = StoredSourceRevision::new(
        STANDARD_SOURCE_V5_BUNDLE_ID,
        STANDARD_SOURCE_V5_REVISION_ID,
        Some(STANDARD_SOURCE_V4_REVISION_ID),
        units,
        bundle_hash,
        revision_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let executable = retained_v2_executable(invoke_source, catalogue, &invoke_origins)?;
    if executable.revision().artifact().content_hash() != ACCEPTED_V5_ARTIFACT_DIGEST
        || executable.revision().semantic_hash() != ACCEPTED_V5_SEMANTIC_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let json_executable = retained_json_executable(json_source, catalogue, &json_origins)?;
    let snapshot = StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V5_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        retained_source,
        LANGUAGE_VERSION_IDENTITY,
        catalogue.clone(),
        vec![executable, json_executable],
        origins,
        ACCEPTED_V5_STANDARD_LIBRARY_DIGEST,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let actual_digest = calculate_standard_library_digest(&snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if actual_digest != ACCEPTED_V5_STANDARD_LIBRARY_DIGEST {
        return Err(StandardLibraryError::AcceptedDigestMismatch {
            expected: ACCEPTED_V5_STANDARD_LIBRARY_DIGEST,
            actual: actual_digest,
        });
    }
    Ok(snapshot)
}
pub(super) fn retained_standard_library_v6_snapshot_from_source(
    types_source: &str,
    invoke_source: &str,
    output_source: &str,
    ui_source: &str,
    json_source: &str,
    action_source: &str,
) -> Result<StandardLibrarySnapshot, StandardLibraryError> {
    let manifest = standard_library_v6_manifest()
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
    origins.extend(reconcile_retained_output_source(output_source, catalogue)?);
    origins.extend(reconcile_retained_ui_source(ui_source, catalogue)?);
    let json_origins = reconcile_retained_json_source(json_source, catalogue)?;
    origins.extend(json_origins.iter().cloned());
    let action_origins = reconcile_retained_action_source(action_source, catalogue)?;
    origins.extend(action_origins.iter().cloned());

    let types_content_hash = source_unit_content_digest(types_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let invoke_content_hash = source_unit_content_digest(invoke_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let output_content_hash = source_unit_content_digest(output_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let ui_content_hash = source_unit_content_digest(ui_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let json_content_hash = source_unit_content_digest(json_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let action_content_hash = source_unit_content_digest(action_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if types_content_hash != ACCEPTED_V6_TYPES_CONTENT_DIGEST
        || invoke_content_hash != ACCEPTED_V6_INVOKE_CONTENT_DIGEST
        || output_content_hash != ACCEPTED_V6_OUTPUT_CONTENT_DIGEST
        || ui_content_hash != ACCEPTED_V6_UI_CONTENT_DIGEST
        || json_content_hash != ACCEPTED_V6_JSON_CONTENT_DIGEST
        || action_content_hash != ACCEPTED_V6_ACTION_CONTENT_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let units = vec![
        StoredSourceUnit::new(
            STD_TYPES_SOURCE_UNIT_ID,
            0,
            SOURCE_LOGICAL_PATH,
            types_source,
            types_content_hash,
        ),
        StoredSourceUnit::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            1,
            STD_INVOKE_SOURCE_LOGICAL_PATH,
            invoke_source,
            invoke_content_hash,
        ),
        StoredSourceUnit::new(
            STD_OUTPUT_SOURCE_UNIT_ID,
            2,
            STD_OUTPUT_SOURCE_LOGICAL_PATH,
            output_source,
            output_content_hash,
        ),
        StoredSourceUnit::new(
            STD_UI_SOURCE_UNIT_ID,
            3,
            STD_UI_SOURCE_LOGICAL_PATH,
            ui_source,
            ui_content_hash,
        ),
        StoredSourceUnit::new(
            STD_JSON_SOURCE_UNIT_ID,
            4,
            STD_JSON_SOURCE_LOGICAL_PATH,
            json_source,
            json_content_hash,
        ),
        StoredSourceUnit::new(
            STD_ACTION_SOURCE_UNIT_ID,
            5,
            STD_ACTION_SOURCE_LOGICAL_PATH,
            action_source,
            action_content_hash,
        ),
    ]
    .into_iter()
    .map(|unit| unit.map_err(|source| StandardLibraryError::Revision { source }))
    .collect::<Result<Vec<_>, _>>()?;
    let bundle_hash = source_bundle_digest(&units)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if bundle_hash != ACCEPTED_V6_SOURCE_BUNDLE_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let revision_hash = source_revision_record_digest(
        STANDARD_SOURCE_V6_BUNDLE_ID,
        Some(STANDARD_SOURCE_V5_REVISION_ID),
        bundle_hash,
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if revision_hash != ACCEPTED_V6_SOURCE_REVISION_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let retained_source = StoredSourceRevision::new(
        STANDARD_SOURCE_V6_BUNDLE_ID,
        STANDARD_SOURCE_V6_REVISION_ID,
        Some(STANDARD_SOURCE_V5_REVISION_ID),
        units,
        bundle_hash,
        revision_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let executable = retained_v2_executable(invoke_source, catalogue, &invoke_origins)?;
    if executable.revision().artifact().content_hash() != ACCEPTED_V6_ARTIFACT_DIGEST
        || executable.revision().semantic_hash() != ACCEPTED_V6_SEMANTIC_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let json_executable = retained_json_executable(json_source, catalogue, &json_origins)?;
    let snapshot = StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V6_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        retained_source,
        LANGUAGE_VERSION_IDENTITY,
        catalogue.clone(),
        vec![executable, json_executable],
        origins,
        ACCEPTED_V6_STANDARD_LIBRARY_DIGEST,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let actual_digest = calculate_standard_library_digest(&snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if actual_digest != ACCEPTED_V6_STANDARD_LIBRARY_DIGEST {
        return Err(StandardLibraryError::AcceptedDigestMismatch {
            expected: ACCEPTED_V6_STANDARD_LIBRARY_DIGEST,
            actual: actual_digest,
        });
    }
    Ok(snapshot)
}

pub(super) fn retained_standard_library_v7_snapshot_from_source(
    types_source: &str,
    invoke_source: &str,
    output_source: &str,
    ui_source: &str,
    json_source: &str,
    action_source: &str,
    window_source: &str,
) -> Result<StandardLibrarySnapshot, StandardLibraryError> {
    let manifest = standard_library_v7_manifest()
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
    origins.extend(reconcile_retained_output_source(output_source, catalogue)?);
    origins.extend(reconcile_retained_ui_source(ui_source, catalogue)?);
    let json_origins = reconcile_retained_json_source(json_source, catalogue)?;
    origins.extend(json_origins.iter().cloned());
    origins.extend(reconcile_retained_action_source(action_source, catalogue)?);
    let window_origins = reconcile_retained_window_source(window_source, catalogue)?;
    origins.extend(window_origins.iter().cloned());

    let types_content_hash = source_unit_content_digest(types_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let invoke_content_hash = source_unit_content_digest(invoke_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let output_content_hash = source_unit_content_digest(output_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let ui_content_hash = source_unit_content_digest(ui_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let json_content_hash = source_unit_content_digest(json_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let action_content_hash = source_unit_content_digest(action_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let window_content_hash = source_unit_content_digest(window_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if types_content_hash != ACCEPTED_V7_TYPES_CONTENT_DIGEST
        || invoke_content_hash != ACCEPTED_V7_INVOKE_CONTENT_DIGEST
        || output_content_hash != ACCEPTED_V7_OUTPUT_CONTENT_DIGEST
        || ui_content_hash != ACCEPTED_V7_UI_CONTENT_DIGEST
        || json_content_hash != ACCEPTED_V7_JSON_CONTENT_DIGEST
        || action_content_hash != ACCEPTED_V7_ACTION_CONTENT_DIGEST
        || window_content_hash != ACCEPTED_V7_WINDOW_CONTENT_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let units = vec![
        StoredSourceUnit::new(
            STD_TYPES_SOURCE_UNIT_ID,
            0,
            SOURCE_LOGICAL_PATH,
            types_source,
            types_content_hash,
        ),
        StoredSourceUnit::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            1,
            STD_INVOKE_SOURCE_LOGICAL_PATH,
            invoke_source,
            invoke_content_hash,
        ),
        StoredSourceUnit::new(
            STD_OUTPUT_SOURCE_UNIT_ID,
            2,
            STD_OUTPUT_SOURCE_LOGICAL_PATH,
            output_source,
            output_content_hash,
        ),
        StoredSourceUnit::new(
            STD_UI_SOURCE_UNIT_ID,
            3,
            STD_UI_SOURCE_LOGICAL_PATH,
            ui_source,
            ui_content_hash,
        ),
        StoredSourceUnit::new(
            STD_JSON_SOURCE_UNIT_ID,
            4,
            STD_JSON_SOURCE_LOGICAL_PATH,
            json_source,
            json_content_hash,
        ),
        StoredSourceUnit::new(
            STD_ACTION_SOURCE_UNIT_ID,
            5,
            STD_ACTION_SOURCE_LOGICAL_PATH,
            action_source,
            action_content_hash,
        ),
        StoredSourceUnit::new(
            STD_WINDOW_SOURCE_UNIT_ID,
            6,
            STD_WINDOW_SOURCE_LOGICAL_PATH,
            window_source,
            window_content_hash,
        ),
    ]
    .into_iter()
    .map(|unit| unit.map_err(|source| StandardLibraryError::Revision { source }))
    .collect::<Result<Vec<_>, _>>()?;
    let bundle_hash = source_bundle_digest(&units)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if bundle_hash != ACCEPTED_V7_SOURCE_BUNDLE_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let revision_hash = source_revision_record_digest(
        STANDARD_SOURCE_V7_BUNDLE_ID,
        Some(STANDARD_SOURCE_V6_REVISION_ID),
        bundle_hash,
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if revision_hash != ACCEPTED_V7_SOURCE_REVISION_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let retained_source = StoredSourceRevision::new(
        STANDARD_SOURCE_V7_BUNDLE_ID,
        STANDARD_SOURCE_V7_REVISION_ID,
        Some(STANDARD_SOURCE_V6_REVISION_ID),
        units,
        bundle_hash,
        revision_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;

    let executable = retained_v2_executable(invoke_source, catalogue, &invoke_origins)?;
    if executable.revision().artifact().content_hash() != ACCEPTED_V7_ARTIFACT_DIGEST
        || executable.revision().semantic_hash() != ACCEPTED_V7_SEMANTIC_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let json_executable = retained_json_executable(json_source, catalogue, &json_origins)?;
    let window_executable = retained_window_executable(window_source, catalogue, &window_origins)?;
    let snapshot = StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V7_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        retained_source,
        LANGUAGE_VERSION_IDENTITY,
        catalogue.clone(),
        vec![executable, json_executable, window_executable],
        origins,
        ACCEPTED_V7_STANDARD_LIBRARY_DIGEST,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let actual_digest = calculate_standard_library_digest(&snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if actual_digest != ACCEPTED_V7_STANDARD_LIBRARY_DIGEST {
        return Err(StandardLibraryError::AcceptedDigestMismatch {
            expected: ACCEPTED_V7_STANDARD_LIBRARY_DIGEST,
            actual: actual_digest,
        });
    }
    Ok(snapshot)
}
#[allow(clippy::too_many_arguments)]
pub(super) fn retained_standard_library_v8_snapshot_from_source(
    types_source: &str,
    invoke_source: &str,
    output_source: &str,
    ui_source: &str,
    json_source: &str,
    action_source: &str,
    window_source: &str,
    data_source: &str,
) -> Result<StandardLibrarySnapshot, StandardLibraryError> {
    let manifest = standard_library_v8_manifest()
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
    origins.extend(reconcile_retained_output_source(output_source, catalogue)?);
    origins.extend(reconcile_retained_ui_source(ui_source, catalogue)?);
    let json_origins = reconcile_retained_json_source(json_source, catalogue)?;
    origins.extend(json_origins.iter().cloned());
    origins.extend(reconcile_retained_action_source(action_source, catalogue)?);
    let window_origins = reconcile_retained_window_source(window_source, catalogue)?;
    origins.extend(window_origins.iter().cloned());
    let data_origins = reconcile_retained_data_source(data_source, catalogue)?;
    origins.extend(data_origins.iter().cloned());

    let types_content_hash = source_unit_content_digest(types_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let invoke_content_hash = source_unit_content_digest(invoke_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let output_content_hash = source_unit_content_digest(output_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let ui_content_hash = source_unit_content_digest(ui_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let json_content_hash = source_unit_content_digest(json_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let action_content_hash = source_unit_content_digest(action_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let window_content_hash = source_unit_content_digest(window_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let data_content_hash = source_unit_content_digest(data_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if types_content_hash != ACCEPTED_V8_TYPES_CONTENT_DIGEST
        || invoke_content_hash != ACCEPTED_V8_INVOKE_CONTENT_DIGEST
        || output_content_hash != ACCEPTED_V8_OUTPUT_CONTENT_DIGEST
        || ui_content_hash != ACCEPTED_V8_UI_CONTENT_DIGEST
        || json_content_hash != ACCEPTED_V8_JSON_CONTENT_DIGEST
        || action_content_hash != ACCEPTED_V8_ACTION_CONTENT_DIGEST
        || window_content_hash != ACCEPTED_V8_WINDOW_CONTENT_DIGEST
        || data_content_hash != ACCEPTED_V8_DATA_CONTENT_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let units = vec![
        StoredSourceUnit::new(
            STD_TYPES_SOURCE_UNIT_ID,
            0,
            SOURCE_LOGICAL_PATH,
            types_source,
            types_content_hash,
        ),
        StoredSourceUnit::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            1,
            STD_INVOKE_SOURCE_LOGICAL_PATH,
            invoke_source,
            invoke_content_hash,
        ),
        StoredSourceUnit::new(
            STD_OUTPUT_SOURCE_UNIT_ID,
            2,
            STD_OUTPUT_SOURCE_LOGICAL_PATH,
            output_source,
            output_content_hash,
        ),
        StoredSourceUnit::new(
            STD_UI_SOURCE_UNIT_ID,
            3,
            STD_UI_SOURCE_LOGICAL_PATH,
            ui_source,
            ui_content_hash,
        ),
        StoredSourceUnit::new(
            STD_JSON_SOURCE_UNIT_ID,
            4,
            STD_JSON_SOURCE_LOGICAL_PATH,
            json_source,
            json_content_hash,
        ),
        StoredSourceUnit::new(
            STD_ACTION_SOURCE_UNIT_ID,
            5,
            STD_ACTION_SOURCE_LOGICAL_PATH,
            action_source,
            action_content_hash,
        ),
        StoredSourceUnit::new(
            STD_WINDOW_SOURCE_UNIT_ID,
            6,
            STD_WINDOW_SOURCE_LOGICAL_PATH,
            window_source,
            window_content_hash,
        ),
        StoredSourceUnit::new(
            STD_DATA_SOURCE_UNIT_ID,
            7,
            STD_DATA_SOURCE_LOGICAL_PATH,
            data_source,
            data_content_hash,
        ),
    ]
    .into_iter()
    .map(|unit| unit.map_err(|source| StandardLibraryError::Revision { source }))
    .collect::<Result<Vec<_>, _>>()?;
    let bundle_hash = source_bundle_digest(&units)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if bundle_hash != ACCEPTED_V8_SOURCE_BUNDLE_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let revision_hash = source_revision_record_digest(
        STANDARD_SOURCE_V8_BUNDLE_ID,
        Some(STANDARD_SOURCE_V7_REVISION_ID),
        bundle_hash,
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if revision_hash != ACCEPTED_V8_SOURCE_REVISION_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let retained_source = StoredSourceRevision::new(
        STANDARD_SOURCE_V8_BUNDLE_ID,
        STANDARD_SOURCE_V8_REVISION_ID,
        Some(STANDARD_SOURCE_V7_REVISION_ID),
        units,
        bundle_hash,
        revision_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;

    let executable = retained_v2_executable(invoke_source, catalogue, &invoke_origins)?;
    if executable.revision().artifact().content_hash() != ACCEPTED_V8_ARTIFACT_DIGEST
        || executable.revision().semantic_hash() != ACCEPTED_V8_SEMANTIC_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let json_executable = retained_json_executable(json_source, catalogue, &json_origins)?;
    let window_executable = retained_window_executable(window_source, catalogue, &window_origins)?;
    let table_executable =
        retained_terminal_table_executable(data_source, catalogue, &data_origins)?;
    if table_executable.revision().artifact().content_hash() != ACCEPTED_V8_TABLE_ARTIFACT_DIGEST
        || table_executable.revision().semantic_hash() != ACCEPTED_V8_TABLE_SEMANTIC_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let snapshot = StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V8_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        retained_source,
        LANGUAGE_VERSION_IDENTITY,
        catalogue.clone(),
        vec![
            executable,
            json_executable,
            table_executable,
            window_executable,
        ],
        origins,
        ACCEPTED_V8_STANDARD_LIBRARY_DIGEST,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let actual_digest = calculate_standard_library_digest(&snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if actual_digest != ACCEPTED_V8_STANDARD_LIBRARY_DIGEST {
        return Err(StandardLibraryError::AcceptedDigestMismatch {
            expected: ACCEPTED_V8_STANDARD_LIBRARY_DIGEST,
            actual: actual_digest,
        });
    }
    Ok(snapshot)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn retained_standard_library_v9_snapshot_from_source(
    types_source: &str,
    invoke_source: &str,
    output_source: &str,
    ui_source: &str,
    json_source: &str,
    action_source: &str,
    window_source: &str,
    data_source: &str,
    constructors_source: &str,
) -> Result<StandardLibrarySnapshot, StandardLibraryError> {
    let manifest = standard_library_v9_manifest()
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
    origins.extend(reconcile_retained_output_source(output_source, catalogue)?);
    origins.extend(reconcile_retained_ui_source(ui_source, catalogue)?);
    let json_origins = reconcile_retained_json_source(json_source, catalogue)?;
    origins.extend(json_origins.iter().cloned());
    origins.extend(reconcile_retained_action_source(action_source, catalogue)?);
    let window_origins = reconcile_retained_window_source(window_source, catalogue)?;
    origins.extend(window_origins.iter().cloned());
    let data_origins = reconcile_retained_data_source(data_source, catalogue)?;
    origins.extend(data_origins.iter().cloned());
    let constructor_origins =
        reconcile_retained_ui_constructors_source(constructors_source, catalogue)?;
    origins.extend(constructor_origins.iter().cloned());

    let types_content_hash = source_unit_content_digest(types_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let invoke_content_hash = source_unit_content_digest(invoke_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let output_content_hash = source_unit_content_digest(output_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let ui_content_hash = source_unit_content_digest(ui_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let json_content_hash = source_unit_content_digest(json_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let action_content_hash = source_unit_content_digest(action_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let window_content_hash = source_unit_content_digest(window_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let data_content_hash = source_unit_content_digest(data_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let constructors_content_hash = source_unit_content_digest(constructors_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if types_content_hash != ACCEPTED_V9_TYPES_CONTENT_DIGEST
        || invoke_content_hash != ACCEPTED_V9_INVOKE_CONTENT_DIGEST
        || output_content_hash != ACCEPTED_V9_OUTPUT_CONTENT_DIGEST
        || ui_content_hash != ACCEPTED_V9_UI_CONTENT_DIGEST
        || json_content_hash != ACCEPTED_V9_JSON_CONTENT_DIGEST
        || action_content_hash != ACCEPTED_V9_ACTION_CONTENT_DIGEST
        || window_content_hash != ACCEPTED_V9_WINDOW_CONTENT_DIGEST
        || data_content_hash != ACCEPTED_V9_DATA_CONTENT_DIGEST
        || constructors_content_hash != ACCEPTED_V9_UI_CONSTRUCTORS_CONTENT_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let units = vec![
        StoredSourceUnit::new(
            STD_TYPES_SOURCE_UNIT_ID,
            0,
            SOURCE_LOGICAL_PATH,
            types_source,
            types_content_hash,
        ),
        StoredSourceUnit::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            1,
            STD_INVOKE_SOURCE_LOGICAL_PATH,
            invoke_source,
            invoke_content_hash,
        ),
        StoredSourceUnit::new(
            STD_OUTPUT_SOURCE_UNIT_ID,
            2,
            STD_OUTPUT_SOURCE_LOGICAL_PATH,
            output_source,
            output_content_hash,
        ),
        StoredSourceUnit::new(
            STD_UI_SOURCE_UNIT_ID,
            3,
            STD_UI_SOURCE_LOGICAL_PATH,
            ui_source,
            ui_content_hash,
        ),
        StoredSourceUnit::new(
            STD_JSON_SOURCE_UNIT_ID,
            4,
            STD_JSON_SOURCE_LOGICAL_PATH,
            json_source,
            json_content_hash,
        ),
        StoredSourceUnit::new(
            STD_ACTION_SOURCE_UNIT_ID,
            5,
            STD_ACTION_SOURCE_LOGICAL_PATH,
            action_source,
            action_content_hash,
        ),
        StoredSourceUnit::new(
            STD_WINDOW_SOURCE_UNIT_ID,
            6,
            STD_WINDOW_SOURCE_LOGICAL_PATH,
            window_source,
            window_content_hash,
        ),
        StoredSourceUnit::new(
            STD_DATA_SOURCE_UNIT_ID,
            7,
            STD_DATA_SOURCE_LOGICAL_PATH,
            data_source,
            data_content_hash,
        ),
        StoredSourceUnit::new(
            STD_UI_CONSTRUCTORS_SOURCE_UNIT_ID,
            8,
            STD_UI_CONSTRUCTORS_SOURCE_LOGICAL_PATH,
            constructors_source,
            constructors_content_hash,
        ),
    ]
    .into_iter()
    .map(|unit| unit.map_err(|source| StandardLibraryError::Revision { source }))
    .collect::<Result<Vec<_>, _>>()?;
    let bundle_hash = source_bundle_digest(&units)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if bundle_hash != ACCEPTED_V9_SOURCE_BUNDLE_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let revision_hash = source_revision_record_digest(
        STANDARD_SOURCE_V9_BUNDLE_ID,
        Some(STANDARD_SOURCE_V8_REVISION_ID),
        bundle_hash,
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if revision_hash != ACCEPTED_V9_SOURCE_REVISION_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let retained_source = StoredSourceRevision::new(
        STANDARD_SOURCE_V9_BUNDLE_ID,
        STANDARD_SOURCE_V9_REVISION_ID,
        Some(STANDARD_SOURCE_V8_REVISION_ID),
        units,
        bundle_hash,
        revision_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;

    let executable = retained_v2_executable(invoke_source, catalogue, &invoke_origins)?;
    if executable.revision().artifact().content_hash() != ACCEPTED_V9_ARTIFACT_DIGEST
        || executable.revision().semantic_hash() != ACCEPTED_V9_SEMANTIC_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let json_executable = retained_json_executable(json_source, catalogue, &json_origins)?;
    let window_executable = retained_window_executable(window_source, catalogue, &window_origins)?;
    let table_executable =
        retained_terminal_table_executable(data_source, catalogue, &data_origins)?;
    if table_executable.revision().artifact().content_hash() != ACCEPTED_V9_TABLE_ARTIFACT_DIGEST
        || table_executable.revision().semantic_hash() != ACCEPTED_V9_TABLE_SEMANTIC_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let constructor_executables =
        retained_ui_constructor_executables(constructors_source, catalogue, &constructor_origins)?;
    let snapshot = StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V9_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        retained_source,
        LANGUAGE_VERSION_IDENTITY,
        catalogue.clone(),
        [
            vec![
                executable,
                json_executable,
                table_executable,
                window_executable,
            ],
            constructor_executables,
        ]
        .concat(),
        origins,
        ACCEPTED_V9_STANDARD_LIBRARY_DIGEST,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let actual_digest = calculate_standard_library_digest(&snapshot)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if actual_digest != ACCEPTED_V9_STANDARD_LIBRARY_DIGEST {
        return Err(StandardLibraryError::AcceptedDigestMismatch {
            expected: ACCEPTED_V9_STANDARD_LIBRARY_DIGEST,
            actual: actual_digest,
        });
    }
    Ok(snapshot)
}
