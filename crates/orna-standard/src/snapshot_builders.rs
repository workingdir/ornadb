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
pub(super) fn retained_standard_library_v10_snapshot_from_source(
    cli_source: &str,
) -> Result<StandardLibrarySnapshot, StandardLibraryError> {
    let parent = retained_standard_library_v9_snapshot()?;
    let manifest = standard_library_v10_manifest()
        .map_err(|source| StandardLibraryError::Manifest { source })?;
    let cli_content_hash = source_unit_content_digest(cli_source)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if cli_content_hash != ACCEPTED_V10_CLI_CONTENT_DIGEST {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let cli_unit = StoredSourceUnit::new(
        STD_CLI_SOURCE_UNIT_ID,
        parent.source().units().len() as u32,
        STD_CLI_SOURCE_LOGICAL_PATH,
        cli_source,
        cli_content_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let parsed = orna_syntax::parse(cli_source);
    if !parsed.diagnostics().is_empty()
        || parsed.syntax().text() != cli_source
        || parsed.schemas().len() != 1
        || parsed.client_functions().len() != 1
        || !parsed.object_types().is_empty()
        || !parsed.field_renames().is_empty()
        || !parsed.server_functions().is_empty()
        || !parsed.primitive_value_types().is_empty()
        || !parsed.opaque_value_types().is_empty()
        || !parsed.record_value_types().is_empty()
        || !parsed.enum_types().is_empty()
        || !parsed.type_exports().is_empty()
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let origin = |span: &orna_syntax::SourceSpan| -> Result<SourceOrigin, StandardLibraryError> {
        let start =
            u32::try_from(span.start).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        let end =
            u32::try_from(span.end).map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
        SourceOrigin::new(STD_CLI_SOURCE_UNIT_ID, start, end)
            .map_err(|source| StandardLibraryError::Revision { source })
    };
    let cli_origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(STD_CLI_SCHEMA_ID),
            origin(&parsed.schemas()[0].span)?,
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Function(STD_CLI_REPL_FUNCTION_ID),
            origin(&parsed.client_functions()[0].span)?,
        ),
    ];
    let checked = orna_compiler::check_standard_cli_repl(
        &parsed.client_functions()[0],
        manifest.catalogue(),
        &cli_origins,
        &cli_unit,
    )
    .map_err(|_| StandardLibraryError::RetainedSourceMismatch)?;
    let revision = FunctionRevisionRecord::new(
        checked.function_id(),
        checked.revision_id(),
        checked.revision_number(),
        checked.declaration_origin(),
        checked.declaration_content_hash(),
        checked.semantic_hash(),
        checked.language_version(),
        checked.artifact().clone(),
    )
    .map_err(|source| StandardLibraryError::Revision { source })?
    .with_semantic_hash_version(checked.semantic_hash_version());
    let executable = StandardExecutable::new(
        checked.function_id(),
        revision,
        checked.references().to_vec(),
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    if checked.artifact().content_hash() != ACCEPTED_V10_CLI_ARTIFACT_DIGEST
        || checked.semantic_hash() != ACCEPTED_V10_CLI_SEMANTIC_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let mut units = parent.source().units().to_vec();
    units.push(cli_unit);
    let bundle_hash = source_bundle_digest(&units)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    let revision_hash = source_revision_record_digest(
        STANDARD_SOURCE_V10_BUNDLE_ID,
        Some(STANDARD_SOURCE_V9_REVISION_ID),
        bundle_hash,
    )
    .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if bundle_hash != ACCEPTED_V10_SOURCE_BUNDLE_DIGEST
        || revision_hash != ACCEPTED_V10_SOURCE_REVISION_DIGEST
    {
        return Err(StandardLibraryError::RetainedSourceMismatch);
    }
    let source = StoredSourceRevision::new(
        STANDARD_SOURCE_V10_BUNDLE_ID,
        STANDARD_SOURCE_V10_REVISION_ID,
        Some(STANDARD_SOURCE_V9_REVISION_ID),
        units,
        bundle_hash,
        revision_hash,
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let mut origins = parent.origins().to_vec();
    origins.extend(cli_origins);
    let mut executables = parent.executables().to_vec();
    executables.push(executable);
    let provisional = StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V10_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        source,
        LANGUAGE_VERSION_IDENTITY,
        manifest.catalogue().clone(),
        executables,
        origins,
        Sha256Digest::from_bytes([0; 32]),
    )
    .map_err(|source| StandardLibraryError::Revision { source })?;
    let digest = calculate_standard_library_digest(&provisional)
        .map_err(|source| StandardLibraryError::CanonicalHash { source })?;
    if digest != ACCEPTED_V10_STANDARD_LIBRARY_DIGEST {
        return Err(StandardLibraryError::AcceptedDigestMismatch {
            expected: ACCEPTED_V10_STANDARD_LIBRARY_DIGEST,
            actual: digest,
        });
    }
    StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V10_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        provisional.source().clone(),
        LANGUAGE_VERSION_IDENTITY,
        provisional.catalogue().clone(),
        provisional.executables().to_vec(),
        provisional.origins().to_vec(),
        digest,
    )
    .map_err(|source| StandardLibraryError::Revision { source })
}
