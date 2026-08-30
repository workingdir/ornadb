use super::*;

pub(super) fn stored_v2_unit(
    id: SourceUnitId,
    ordinal: u32,
    path: &str,
    content: &str,
) -> StoredSourceUnit {
    StoredSourceUnit::new(
        id,
        ordinal,
        path,
        content,
        source_unit_content_digest(content).unwrap(),
    )
    .unwrap()
}

fn standard_v2_types_catalogue() -> CatalogueSnapshot {
    let integer = ValueTypeDefinition::primitive(
        STD_INTEGER_TYPE_ID,
        QualifiedSemanticName::new(["std", "types", "integer"]).unwrap(),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.integer@1",
    );
    let qualified = TypeBinding::qualified(
        QualifiedSemanticName::new(["std", "integer"]).unwrap(),
        integer.id(),
    )
    .unwrap();
    let prelude =
        TypeBinding::prelude(PreludeTypeName::new(["integer"]).unwrap(), integer.id()).unwrap();
    CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0x21; 16]),
        vec![
            SchemaDefinition::new(
                SchemaId::from_bytes([1; 16]),
                QualifiedSemanticName::new(["std"]).unwrap(),
            ),
            SchemaDefinition::new(
                SchemaId::from_bytes([2; 16]),
                QualifiedSemanticName::new(["std", "types"]).unwrap(),
            ),
        ],
        vec![],
        vec![integer],
        vec![qualified, prelude],
    )
    .unwrap()
}

fn standard_v2_catalogue(with_invoke: bool) -> CatalogueSnapshot {
    let catalogue = standard_v2_types_catalogue();
    if !with_invoke {
        return catalogue;
    }
    let echo = FunctionDefinition::new(
        STD_INVOKE_ECHO_FUNCTION_ID,
        QualifiedSemanticName::new(["std", "invoke", "echo"]).unwrap(),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            STD_INVOKE_ECHO_PARAMETER_ID,
            "p_value",
            0,
            ResolvedType::scalar(StandardScalar::Integer),
            None,
        )],
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    let mut schemas = catalogue.schemas().to_vec();
    schemas.push(SchemaDefinition::new(
        STD_INVOKE_SCHEMA_ID,
        QualifiedSemanticName::new(["std", "invoke"]).unwrap(),
    ));
    CatalogueSnapshot::new_with_functions_and_types(
        CatalogueRevisionId::from_bytes([0x21; 16]),
        schemas,
        vec![],
        catalogue.value_types().to_vec(),
        catalogue.type_bindings().to_vec(),
        vec![echo],
    )
    .unwrap()
}

pub(super) fn standard_v2_types_origins(
    catalogue: &CatalogueSnapshot,
    parsed: &ParsedSourceUnit,
) -> Vec<DefinitionOrigin> {
    let origin = |identity: DefinitionIdentity, span: &SourceSpan| -> DefinitionOrigin {
        DefinitionOrigin::new(
            identity,
            SourceOrigin::new(
                STD_TYPES_SOURCE_UNIT_ID,
                u32::try_from(span.start).unwrap(),
                u32::try_from(span.end).unwrap(),
            )
            .unwrap(),
        )
    };
    let mut origins = Vec::new();
    for declaration in parsed.parsed().schemas() {
        let name = unquoted_semantic_name(&declaration.name).unwrap();
        let definition = catalogue.schema_by_name(&name).unwrap();
        origins.push(origin(
            DefinitionIdentity::Schema(definition.id()),
            &declaration.span,
        ));
    }
    for declaration in parsed.parsed().primitive_value_types() {
        let name = unquoted_semantic_name(&declaration.name).unwrap();
        let definition = catalogue.value_type_by_name(&name).unwrap();
        origins.push(origin(
            DefinitionIdentity::ValueType(definition.id()),
            &declaration.span,
        ));
    }
    for declaration in parsed.parsed().type_exports() {
        let target = match &declaration.target {
            TypeExportTarget::Qualified { name } => {
                TypeLookupName::qualified(unquoted_semantic_name(name).unwrap())
            }
            TypeExportTarget::Prelude { words, .. } => {
                TypeLookupName::prelude(unquoted_prelude_name(words).unwrap())
            }
        };
        let binding = catalogue.type_binding_by_name(&target).unwrap();
        origins.push(origin(
            DefinitionIdentity::TypeBinding(binding.id()),
            &declaration.span,
        ));
    }
    origins
}

pub(super) fn standard_v2_invoke_origins(source: &str) -> Vec<DefinitionOrigin> {
    let report =
        parse_bundle(&SourceBundle::new([SourceUnit::new("std/invoke.orna", source)]).unwrap());
    assert!(report.diagnostics().is_empty());
    let parsed = &report.units()[0];
    let schema_span = &parsed.parsed().schemas()[0].span;
    let mut origins = standard_parameter_echo_origins(source);
    origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Schema(STD_INVOKE_SCHEMA_ID),
        SourceOrigin::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            u32::try_from(schema_span.start).unwrap(),
            u32::try_from(schema_span.end).unwrap(),
        )
        .unwrap(),
    ));
    origins
}

pub(super) fn standard_v2_executable(
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
) -> StandardExecutable {
    let checked = check_echo(STD_INVOKE_SOURCE).unwrap();
    let function = catalogue
        .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
        .unwrap();
    let function_origin = origins
        .iter()
        .find(|origin| {
            origin.identity() == DefinitionIdentity::Function(STD_INVOKE_ECHO_FUNCTION_ID)
        })
        .unwrap()
        .source();
    let declaration_content_hash = function_declaration_digest(
        &STD_INVOKE_SOURCE.as_bytes()
            [function_origin.byte_start() as usize..function_origin.byte_end() as usize],
    )
    .unwrap();
    let semantic = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version2,
        function,
        "orna.language/1",
        checked.artifact(),
        &[],
        checked.references(),
    )
    .unwrap();
    let revision = FunctionRevisionRecord::new(
        checked.function_id(),
        checked.revision_id(),
        STD_INVOKE_ECHO_REVISION_NUMBER,
        function_origin,
        declaration_content_hash,
        semantic,
        "orna.language/1",
        checked.artifact().clone(),
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    StandardExecutable::new(
        checked.function_id(),
        revision,
        checked.references().to_vec(),
    )
    .unwrap()
}

fn standard_v2_units() -> (StoredSourceUnit, StoredSourceUnit) {
    (
        stored_v2_unit(
            STD_TYPES_SOURCE_UNIT_ID,
            0,
            "std/types.orna",
            STANDARD_V2_TYPES_SOURCE,
        ),
        stored_v2_unit(
            STD_INVOKE_SOURCE_UNIT_ID,
            1,
            "std/invoke.orna",
            STD_INVOKE_SOURCE,
        ),
    )
}

/// The compiled canonical V2 standard-library digest for the exact test
/// inputs (`STANDARD_V2_TYPES_SOURCE`, `STD_INVOKE_SOURCE`, the fixed
/// identities, catalogue, executable, and origins). Computed by the
/// canonical encoder.
const STANDARD_V2_CANONICAL_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    115, 202, 159, 209, 255, 174, 218, 69, 195, 114, 168, 108, 210, 7, 50, 127, 176, 149, 134, 145,
    229, 113, 139, 179, 237, 228, 75, 75, 94, 20, 52, 52,
]);

fn standard_v2_source(units: Vec<StoredSourceUnit>) -> StoredSourceRevision {
    let bundle_hash = source_bundle_digest(&units).unwrap();
    StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x41; 16]),
        SourceRevisionId::from_bytes([0x42; 16]),
        Some(SourceRevisionId::from_bytes([0x43; 16])),
        units,
        bundle_hash,
        source_revision_record_digest(
            SourceBundleId::from_bytes([0x41; 16]),
            Some(SourceRevisionId::from_bytes([0x43; 16])),
            bundle_hash,
        )
        .unwrap(),
    )
    .unwrap()
}

fn build_standard_v2_snapshot(
    units: Vec<StoredSourceUnit>,
    catalogue: CatalogueSnapshot,
    origins: Vec<DefinitionOrigin>,
    executables: Vec<StandardExecutable>,
    digest: Sha256Digest,
) -> StandardLibrarySnapshot {
    StandardLibrarySnapshot::new_with_executables(
        StandardLibraryRevisionId::from_bytes([0x44; 16]),
        StandardLibraryDigestVersion::Version2,
        standard_v2_source(units),
        "orna.language/1",
        catalogue,
        executables,
        origins,
        digest,
    )
    .unwrap()
}

fn standard_v2_parts() -> (
    Vec<StoredSourceUnit>,
    CatalogueSnapshot,
    Vec<DefinitionOrigin>,
    Vec<StandardExecutable>,
) {
    let (types_unit, invoke_unit) = standard_v2_units();
    let catalogue = standard_v2_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let executable = standard_v2_executable(&catalogue, &origins);
    (
        vec![types_unit, invoke_unit],
        catalogue,
        origins,
        vec![executable],
    )
}

/// Runs the V2 source reconcile directly on raw stored facts, without the
/// separate digest-verification gate.
fn check_v2_parts(
    units: Vec<StoredSourceUnit>,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    check_standard_library_source_v2_parts(
        &standard_v2_source(units),
        catalogue,
        origins,
        executables,
    )
}

pub(super) fn verified_standard_v2_snapshot() -> VerifiedStandardLibrarySnapshot {
    let (units, catalogue, origins, executables) = standard_v2_parts();
    verify_standard_library_v2_snapshot(build_standard_v2_snapshot(
        units,
        catalogue,
        origins,
        executables,
        STANDARD_V2_CANONICAL_DIGEST,
    ))
    .unwrap()
}

#[test]
fn reconciles_the_exact_v2_standard_executable_bundle() {
    let verified = verified_standard_v2_snapshot();
    let checked = check_standard_library_source(&verified).unwrap();

    assert_eq!(checked.schemas().len(), 2);
    assert_eq!(checked.value_types().len(), 1);
    assert_eq!(checked.type_bindings().len(), 2);

    let executable = checked.checked_executable().unwrap();
    assert_eq!(executable.function_id(), STD_INVOKE_ECHO_FUNCTION_ID);
    assert_eq!(executable.parameter_ids(), &[STD_INVOKE_ECHO_PARAMETER_ID]);
    assert_eq!(
        executable.revision_id(),
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID
    );
    assert_eq!(
        executable.revision_number(),
        STD_INVOKE_ECHO_REVISION_NUMBER
    );
    assert_eq!(
        executable.semantic_hash_version(),
        FunctionSemanticHashVersion::Version2
    );
    assert_eq!(executable.language_version(), "orna.language/1");

    let stored = &verified.executables()[0];
    assert_eq!(executable.function_id(), stored.function());
    assert_eq!(executable.revision_id(), stored.revision().id());
    assert_eq!(
        executable.revision_number(),
        stored.revision().revision_number()
    );
    assert_eq!(
        executable.semantic_hash_version(),
        stored.revision().semantic_hash_version()
    );
    assert_eq!(
        executable.language_version(),
        stored.revision().language_version()
    );
    assert_eq!(executable.artifact(), stored.revision().artifact());
    assert_eq!(executable.references(), stored.references());
    assert_eq!(
        executable.declaration_origin(),
        stored.revision().declaration_origin()
    );
    assert_eq!(
        executable.declaration_content_hash(),
        stored.revision().declaration_content_hash()
    );
    assert_eq!(
        executable.semantic_hash(),
        stored.revision().semantic_hash()
    );

    assert_eq!(
        executable.schema_origin().source_unit(),
        STD_INVOKE_SOURCE_UNIT_ID
    );
    assert_eq!(
        executable.function_origin(),
        executable.declaration_origin()
    );
    assert_eq!(
        executable.parameter_origins()[0].source_unit(),
        STD_INVOKE_SOURCE_UNIT_ID
    );
    let stored_schema_origin = verified
        .origins()
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Schema(STD_INVOKE_SCHEMA_ID))
        .unwrap()
        .source();
    assert_eq!(executable.schema_origin(), stored_schema_origin);
    assert_eq!(verified.origins().len(), 8);
    assert_eq!(
        &STD_INVOKE_SOURCE[executable.schema_origin().byte_start() as usize
            ..executable.schema_origin().byte_end() as usize],
        "CREATE SCHEMA std.invoke;"
    );
}

#[test]
fn version_one_keeps_the_type_only_contract_without_executable_facts() {
    let verified = verified_standard_library_for_relational_test();
    let checked = check_standard_library_source(&verified).unwrap();
    assert!(checked.checked_executable().is_none());
    assert_eq!(checked.schemas().len(), 2);
    assert_eq!(checked.value_types().len(), 1);
    assert_eq!(checked.type_bindings().len(), 2);
}

#[test]
fn rejects_v1_source_unit_identity_mutations() {
    let verified = verified_standard_library_for_relational_test();
    assert!(check_standard_library_source(&verified).is_ok());
    let stored = &verified.source().units()[0];

    for (label, id, logical_path) in [
        (
            "stable source-unit id",
            SourceUnitId::from_bytes([0x55; 16]),
            stored.logical_path(),
        ),
        ("logical path", stored.id(), "std/renamed.orna"),
    ] {
        let mutated = verified_v1_with_source_unit_identity(&verified, id, logical_path, 0);
        let error = check_standard_library_source(&mutated).unwrap_err();
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "{label}: unexpected rejection: {error}"
        );
    }

    let ordinal = StoredSourceUnit::new(
        STANDARD_SOURCE_UNIT_ID,
        1,
        stored.logical_path(),
        stored.content(),
        stored.content_hash(),
    )
    .unwrap();
    assert!(matches!(
        check_standard_library_source_v1_identity(&ordinal),
        Err(StandardLibraryCheckError::SourceMismatch)
    ));
}

fn verified_v1_with_source_unit_identity(
    verified: &VerifiedStandardLibrarySnapshot,
    id: SourceUnitId,
    logical_path: &str,
    ordinal: u32,
) -> VerifiedStandardLibrarySnapshot {
    let stored = &verified.source().units()[0];
    let unit = StoredSourceUnit::new(
        id,
        ordinal,
        logical_path,
        stored.content(),
        stored.content_hash(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
    let source = StoredSourceRevision::new(
        verified.source().bundle(),
        verified.source().id(),
        verified.source().parent(),
        vec![unit],
        bundle_hash,
        source_revision_record_digest(
            verified.source().bundle(),
            verified.source().parent(),
            bundle_hash,
        )
        .unwrap(),
    )
    .unwrap();
    let origins = verified
        .origins()
        .iter()
        .map(|origin| {
            let source_origin = origin.source();
            let source_unit = if source_origin.source_unit() == stored.id() {
                id
            } else {
                source_origin.source_unit()
            };
            DefinitionOrigin::new(
                origin.identity(),
                SourceOrigin::new(
                    source_unit,
                    source_origin.byte_start(),
                    source_origin.byte_end(),
                )
                .unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let provisional = StandardLibrarySnapshot::new(
        verified.revision(),
        verified.digest_version(),
        source,
        verified.language_version(),
        verified.catalogue().clone(),
        origins,
        Sha256Digest::from_bytes([0; 32]),
    )
    .unwrap();
    let digest = calculate_standard_library_digest(&provisional).unwrap();
    verify_standard_library_snapshot(
        StandardLibrarySnapshot::new(
            provisional.revision(),
            provisional.digest_version(),
            provisional.source().clone(),
            provisional.language_version(),
            provisional.catalogue().clone(),
            provisional.origins().to_vec(),
            digest,
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn rejects_a_v2_bundle_with_the_wrong_unit_identity() {
    let (_, invoke_unit) = standard_v2_units();
    let catalogue = standard_v2_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let executable = standard_v2_executable(&catalogue, &origins);
    let types_unit = stored_v2_unit(
        SourceUnitId::from_bytes([0x77; 16]),
        0,
        "std/types.orna",
        STANDARD_V2_TYPES_SOURCE,
    );
    let error = check_v2_parts(
        vec![types_unit, invoke_unit],
        &catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(
        matches!(error, StandardLibraryCheckError::SourceMismatch),
        "unexpected rejection: {error}"
    );
}

#[test]
fn rejects_a_v2_bundle_with_swapped_unit_order() {
    let catalogue = standard_v2_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let executable = standard_v2_executable(&catalogue, &origins);
    let types_unit = stored_v2_unit(
        STD_TYPES_SOURCE_UNIT_ID,
        1,
        "std/types.orna",
        STANDARD_V2_TYPES_SOURCE,
    );
    let invoke_unit = stored_v2_unit(
        STD_INVOKE_SOURCE_UNIT_ID,
        0,
        "std/invoke.orna",
        STD_INVOKE_SOURCE,
    );
    let error = check_v2_parts(
        vec![invoke_unit, types_unit],
        &catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(
        matches!(error, StandardLibraryCheckError::SourceMismatch),
        "unexpected rejection: {error}"
    );
}

#[test]
fn rejects_a_v2_bundle_with_the_wrong_logical_path() {
    let (types_unit, _) = standard_v2_units();
    let catalogue = standard_v2_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let executable = standard_v2_executable(&catalogue, &origins);
    let invoke_unit = stored_v2_unit(
        STD_INVOKE_SOURCE_UNIT_ID,
        1,
        "std/invocation.orna",
        STD_INVOKE_SOURCE,
    );
    let error = check_v2_parts(
        vec![types_unit, invoke_unit],
        &catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(
        matches!(error, StandardLibraryCheckError::SourceMismatch),
        "unexpected rejection: {error}"
    );
}

#[test]
fn rejects_a_v2_bundle_with_a_missing_or_extra_unit() {
    let (types_unit, _) = standard_v2_units();
    let catalogue = standard_v2_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let executable = standard_v2_executable(&catalogue, &origins);
    let error = check_v2_parts(
        vec![types_unit],
        &catalogue,
        &origins,
        std::slice::from_ref(&executable),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::SourceUnitCount { actual: 1 }
    ));

    let (types_unit, invoke_unit) = standard_v2_units();
    let extra = stored_v2_unit(
        SourceUnitId::from_bytes([0x78; 16]),
        2,
        "std/extra.orna",
        "CREATE SCHEMA std.extra;",
    );
    let error = check_v2_parts(
        vec![types_unit, invoke_unit, extra],
        &catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::SourceUnitCount { actual: 3 }
    ));
}

#[test]
fn rejects_a_byte_modified_invoke_unit_closed() {
    let (types_unit, _) = standard_v2_units();
    let catalogue = standard_v2_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let executable = standard_v2_executable(&catalogue, &origins);

    // Whitespace-only modification: tokens identical, declaration byte
    // ranges shift, so the stored origins and declaration content hash no
    // longer agree with the retained source.
    let modified = STD_INVOKE_SOURCE.replacen("RETURNS INTEGER", "RETURNS  INTEGER", 1);
    assert_ne!(modified, STD_INVOKE_SOURCE);
    let invoke_unit = stored_v2_unit(STD_INVOKE_SOURCE_UNIT_ID, 1, "std/invoke.orna", &modified);
    let error = check_v2_parts(
        vec![types_unit.clone(), invoke_unit],
        &catalogue,
        &origins,
        std::slice::from_ref(&executable),
    )
    .unwrap_err();
    assert!(
        matches!(error, StandardLibraryCheckError::SourceMismatch),
        "unexpected rejection: {error}"
    );

    // Semantic modification: the echo shape itself is rejected.
    let modified = STD_INVOKE_SOURCE.replacen("p_value INTEGER", "p_value BIGINT", 1);
    let invoke_unit = stored_v2_unit(STD_INVOKE_SOURCE_UNIT_ID, 1, "std/invoke.orna", &modified);
    let error = check_v2_parts(
        vec![types_unit, invoke_unit],
        &catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::UnexpectedParameterType
    ));
}

#[test]
fn rejects_a_v2_bundle_with_the_wrong_source_or_catalogue_names() {
    let (types_unit, _) = standard_v2_units();
    let catalogue = standard_v2_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let executable = standard_v2_executable(&catalogue, &origins);

    // Source schema renamed.
    let invoke_unit = stored_v2_unit(
        STD_INVOKE_SOURCE_UNIT_ID,
        1,
        "std/invoke.orna",
        &STD_INVOKE_SOURCE.replacen("CREATE SCHEMA std.invoke;", "CREATE SCHEMA std.other;", 1),
    );
    let error = check_v2_parts(
        vec![types_unit.clone(), invoke_unit],
        &catalogue,
        &origins,
        std::slice::from_ref(&executable),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::SchemaNameMismatch { .. }
    ));

    // Source function renamed.
    let invoke_unit = stored_v2_unit(
        STD_INVOKE_SOURCE_UNIT_ID,
        1,
        "std/invoke.orna",
        &STD_INVOKE_SOURCE.replacen("std.invoke.echo(", "std.invoke.echo2(", 1),
    );
    let error = check_v2_parts(
        vec![types_unit.clone(), invoke_unit],
        &catalogue,
        &origins,
        std::slice::from_ref(&executable),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::UnexpectedName { .. }
    ));

    // Catalogue schema renamed at the fixed identity (the function name
    // must follow so the catalogue constructor stays valid).
    let mut renamed_schemas = catalogue.schemas().to_vec();
    renamed_schemas[2] = SchemaDefinition::new(
        STD_INVOKE_SCHEMA_ID,
        QualifiedSemanticName::new(["std", "other"]).unwrap(),
    );
    let function = catalogue
        .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
        .unwrap();
    let renamed_function = FunctionDefinition::new(
        function.id(),
        QualifiedSemanticName::new(["std", "other", "echo"]).unwrap(),
        function.domain(),
        function.parameters().to_vec(),
        function.return_type().clone(),
        function.current_revision(),
        function.security(),
        function.transaction(),
        function.volatility(),
    );
    let renamed_catalogue = CatalogueSnapshot::new_with_functions_and_types(
        CatalogueRevisionId::from_bytes([0x21; 16]),
        renamed_schemas,
        vec![],
        catalogue.value_types().to_vec(),
        catalogue.type_bindings().to_vec(),
        vec![renamed_function],
    )
    .unwrap();
    let error = check_v2_parts(
        vec![types_unit.clone(), standard_v2_units().1],
        &renamed_catalogue,
        &origins,
        std::slice::from_ref(&executable),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::SchemaNameMismatch { .. }
    ));

    // Catalogue function renamed at the fixed identity.
    let function = catalogue
        .function_by_id(STD_INVOKE_ECHO_FUNCTION_ID)
        .unwrap();
    let renamed_function = FunctionDefinition::new(
        function.id(),
        QualifiedSemanticName::new(["std", "invoke", "other"]).unwrap(),
        function.domain(),
        function.parameters().to_vec(),
        function.return_type().clone(),
        function.current_revision(),
        function.security(),
        function.transaction(),
        function.volatility(),
    );
    let renamed_catalogue = CatalogueSnapshot::new_with_functions_and_types(
        CatalogueRevisionId::from_bytes([0x21; 16]),
        catalogue.schemas().to_vec(),
        vec![],
        catalogue.value_types().to_vec(),
        catalogue.type_bindings().to_vec(),
        vec![renamed_function],
    )
    .unwrap();
    let error = check_v2_parts(
        vec![types_unit.clone(), standard_v2_units().1],
        &renamed_catalogue,
        &origins,
        std::slice::from_ref(&executable),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::FunctionNameMismatch { .. }
    ));

    // Catalogue parameter renamed at the fixed identity.
    let parameter = function
        .parameter_by_id(STD_INVOKE_ECHO_PARAMETER_ID)
        .unwrap();
    let renamed_function = FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        function.domain(),
        vec![ParameterDefinition::new(
            parameter.id(),
            "p_other",
            parameter.ordinal(),
            parameter.resolved_type(),
            parameter.default_expression(),
        )],
        function.return_type().clone(),
        function.current_revision(),
        function.security(),
        function.transaction(),
        function.volatility(),
    );
    let renamed_catalogue = CatalogueSnapshot::new_with_functions_and_types(
        CatalogueRevisionId::from_bytes([0x21; 16]),
        catalogue.schemas().to_vec(),
        vec![],
        catalogue.value_types().to_vec(),
        catalogue.type_bindings().to_vec(),
        vec![renamed_function],
    )
    .unwrap();
    let error = check_v2_parts(
        vec![types_unit, standard_v2_units().1],
        &renamed_catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::ParameterNameMismatch { .. }
    ));
}

#[test]
fn rejects_wrong_v2_origin_ranges_closed() {
    let (types_unit, invoke_unit) = standard_v2_units();
    let catalogue = standard_v2_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let assert_rejected = |error: StandardLibraryCheckError| {
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "unexpected rejection: {error}"
        );
    };
    let base_origins = standard_v2_types_origins(&catalogue, &parsed_types);

    // Wrong schema origin range.
    let mut origins = base_origins.clone();
    let mut invoke_origins = standard_v2_invoke_origins(STD_INVOKE_SOURCE);
    let schema_origin = invoke_origins
        .iter_mut()
        .find(|origin| origin.identity() == DefinitionIdentity::Schema(STD_INVOKE_SCHEMA_ID))
        .unwrap();
    *schema_origin = DefinitionOrigin::new(
        schema_origin.identity(),
        SourceOrigin::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            schema_origin.source().byte_start() + 1,
            schema_origin.source().byte_end(),
        )
        .unwrap(),
    );
    origins.extend(invoke_origins);
    let executable = standard_v2_executable(&catalogue, &origins);
    assert_rejected(
        check_v2_parts(
            vec![types_unit.clone(), invoke_unit.clone()],
            &catalogue,
            &origins,
            &[executable],
        )
        .unwrap_err(),
    );

    // Wrong function origin range.
    let mut origins = base_origins.clone();
    let mut invoke_origins = standard_v2_invoke_origins(STD_INVOKE_SOURCE);
    let function_origin = invoke_origins
        .iter_mut()
        .find(|origin| {
            origin.identity() == DefinitionIdentity::Function(STD_INVOKE_ECHO_FUNCTION_ID)
        })
        .unwrap();
    *function_origin = DefinitionOrigin::new(
        function_origin.identity(),
        SourceOrigin::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            function_origin.source().byte_start(),
            function_origin.source().byte_end() - 1,
        )
        .unwrap(),
    );
    origins.extend(invoke_origins);
    let executable = standard_v2_executable(&catalogue, &origins);
    assert_rejected(
        check_v2_parts(
            vec![types_unit.clone(), invoke_unit.clone()],
            &catalogue,
            &origins,
            &[executable],
        )
        .unwrap_err(),
    );

    // Wrong parameter origin range.
    let mut origins = base_origins;
    let mut invoke_origins = standard_v2_invoke_origins(STD_INVOKE_SOURCE);
    let parameter_origin = invoke_origins
        .iter_mut()
        .find(|origin| {
            origin.identity()
                == DefinitionIdentity::Parameter {
                    owner: STD_INVOKE_ECHO_FUNCTION_ID,
                    parameter: STD_INVOKE_ECHO_PARAMETER_ID,
                }
        })
        .unwrap();
    *parameter_origin = DefinitionOrigin::new(
        parameter_origin.identity(),
        SourceOrigin::new(
            STD_INVOKE_SOURCE_UNIT_ID,
            parameter_origin.source().byte_start(),
            parameter_origin.source().byte_end() - 1,
        )
        .unwrap(),
    );
    origins.extend(invoke_origins);
    let executable = standard_v2_executable(&catalogue, &origins);
    assert_rejected(
        check_v2_parts(
            vec![types_unit, invoke_unit],
            &catalogue,
            &origins,
            &[executable],
        )
        .unwrap_err(),
    );
}

#[test]
fn rejects_a_wrong_stored_revision_identity_closed() {
    let (types_unit, invoke_unit) = standard_v2_units();
    let catalogue = standard_v2_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let executable = standard_v2_executable(&catalogue, &origins);

    // Rebuild the executable with a different revision identity.
    let wrong_revision = FunctionRevisionId::from_bytes([0x11; 16]);
    let revision = executable.revision().clone();
    let references = executable
        .references()
        .iter()
        .map(|reference| {
            DefinitionReference::new(
                reference.source_function(),
                wrong_revision,
                reference.ordinal(),
                reference.target(),
                reference.kind(),
                reference.source_origin(),
            )
        })
        .collect::<Vec<_>>();
    let revision = FunctionRevisionRecord::new(
        revision.function(),
        wrong_revision,
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        revision.semantic_hash(),
        revision.language_version(),
        revision.artifact().clone(),
    )
    .unwrap()
    .with_semantic_hash_version(revision.semantic_hash_version());
    let executable = StandardExecutable::new(revision.function(), revision, references).unwrap();

    let error = check_v2_parts(
        vec![types_unit, invoke_unit],
        &catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::ExecutableMismatch
    ));
}

#[test]
fn rejects_every_stored_executable_fact_mismatch_closed() {
    let verified = verified_standard_v2_snapshot();
    let checked = check_standard_library_source(&verified)
        .unwrap()
        .checked_executable()
        .unwrap()
        .clone();
    let stored = verified.executables()[0].clone();
    let revision = stored.revision().clone();
    let artifact = revision.artifact().clone();
    let references = stored.references().to_vec();
    let fails = |stored: &StandardExecutable| {
        assert!(
            matches!(
                reconcile_standard_executable(stored, &checked),
                Err(StandardLibraryCheckError::ExecutableMismatch)
            ),
            "expected ExecutableMismatch"
        );
    };

    // Wrong stored function identity.
    let wrong_function = FunctionId::from_bytes([0x55; 16]);
    let mutated = FunctionRevisionRecord::new(
        wrong_function,
        revision.id(),
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        revision.semantic_hash(),
        revision.language_version(),
        artifact.clone(),
    )
    .unwrap()
    .with_semantic_hash_version(revision.semantic_hash_version());
    fails(&StandardExecutable::new(wrong_function, mutated, references.clone()).unwrap());

    // Wrong stored revision identity.
    let wrong_revision = FunctionRevisionId::from_bytes([0x66; 16]);
    let mutated = FunctionRevisionRecord::new(
        revision.function(),
        wrong_revision,
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        revision.semantic_hash(),
        revision.language_version(),
        artifact.clone(),
    )
    .unwrap()
    .with_semantic_hash_version(revision.semantic_hash_version());
    fails(&StandardExecutable::new(revision.function(), mutated, references.clone()).unwrap());

    // Wrong stored revision number.
    let mutated = FunctionRevisionRecord::new(
        revision.function(),
        revision.id(),
        revision.revision_number() + 1,
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        revision.semantic_hash(),
        revision.language_version(),
        artifact.clone(),
    )
    .unwrap()
    .with_semantic_hash_version(revision.semantic_hash_version());
    fails(&StandardExecutable::new(revision.function(), mutated, references.clone()).unwrap());

    // Wrong stored semantic-hash version.
    let mutated = FunctionRevisionRecord::new(
        revision.function(),
        revision.id(),
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        revision.semantic_hash(),
        revision.language_version(),
        artifact.clone(),
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version1);
    fails(&StandardExecutable::new(revision.function(), mutated, references.clone()).unwrap());

    // Wrong stored language version.
    let mutated = FunctionRevisionRecord::new(
        revision.function(),
        revision.id(),
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        revision.semantic_hash(),
        "orna.language/2",
        artifact.clone(),
    )
    .unwrap()
    .with_semantic_hash_version(revision.semantic_hash_version());
    fails(&StandardExecutable::new(revision.function(), mutated, references.clone()).unwrap());

    // Wrong stored declaration origin.
    let mutated = FunctionRevisionRecord::new(
        revision.function(),
        revision.id(),
        revision.revision_number(),
        SourceOrigin::new(STD_INVOKE_SOURCE_UNIT_ID, 0, 1).unwrap(),
        revision.declaration_content_hash(),
        revision.semantic_hash(),
        revision.language_version(),
        artifact.clone(),
    )
    .unwrap()
    .with_semantic_hash_version(revision.semantic_hash_version());
    fails(&StandardExecutable::new(revision.function(), mutated, references.clone()).unwrap());

    // Wrong stored declaration content hash.
    let mutated = FunctionRevisionRecord::new(
        revision.function(),
        revision.id(),
        revision.revision_number(),
        revision.declaration_origin(),
        Sha256Digest::from_bytes([0x11; 32]),
        revision.semantic_hash(),
        revision.language_version(),
        artifact.clone(),
    )
    .unwrap()
    .with_semantic_hash_version(revision.semantic_hash_version());
    fails(&StandardExecutable::new(revision.function(), mutated, references.clone()).unwrap());

    // Wrong stored semantic hash.
    let mutated = FunctionRevisionRecord::new(
        revision.function(),
        revision.id(),
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        Sha256Digest::from_bytes([0x22; 32]),
        revision.language_version(),
        artifact.clone(),
    )
    .unwrap()
    .with_semantic_hash_version(revision.semantic_hash_version());
    fails(&StandardExecutable::new(revision.function(), mutated, references.clone()).unwrap());

    // Wrong stored artifact format.
    let mutated_artifact = ExecutableArtifact::new(
        artifact.kind(),
        "orna.server-parameter-echo2",
        artifact.version(),
        artifact.payload().to_vec(),
        artifact.content_hash(),
    )
    .unwrap();
    let mutated = FunctionRevisionRecord::new(
        revision.function(),
        revision.id(),
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        revision.semantic_hash(),
        revision.language_version(),
        mutated_artifact,
    )
    .unwrap()
    .with_semantic_hash_version(revision.semantic_hash_version());
    fails(&StandardExecutable::new(revision.function(), mutated, references.clone()).unwrap());

    // Wrong stored artifact version.
    let mutated_artifact = ExecutableArtifact::new(
        artifact.kind(),
        artifact.format(),
        artifact.version() + 1,
        artifact.payload().to_vec(),
        artifact.content_hash(),
    )
    .unwrap();
    let mutated = FunctionRevisionRecord::new(
        revision.function(),
        revision.id(),
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        revision.semantic_hash(),
        revision.language_version(),
        mutated_artifact,
    )
    .unwrap()
    .with_semantic_hash_version(revision.semantic_hash_version());
    fails(&StandardExecutable::new(revision.function(), mutated, references.clone()).unwrap());

    // Wrong stored artifact payload.
    let mut payload = artifact.payload().to_vec();
    let last = payload.last_mut().unwrap();
    *last ^= 0xff;
    let mutated_artifact = ExecutableArtifact::new(
        artifact.kind(),
        artifact.format(),
        artifact.version(),
        payload.clone(),
        artifact_payload_digest(&payload).unwrap(),
    )
    .unwrap();
    let mutated = FunctionRevisionRecord::new(
        revision.function(),
        revision.id(),
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        revision.semantic_hash(),
        revision.language_version(),
        mutated_artifact,
    )
    .unwrap()
    .with_semantic_hash_version(revision.semantic_hash_version());
    fails(&StandardExecutable::new(revision.function(), mutated, references.clone()).unwrap());

    // Missing reference.
    fails(
        &StandardExecutable::new(
            revision.function(),
            revision.clone(),
            references[..2].to_vec(),
        )
        .unwrap(),
    );

    // Extra reference.
    let mut extra = references.clone();
    extra.push(DefinitionReference::new(
        STD_INVOKE_ECHO_FUNCTION_ID,
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID,
        3,
        DefinitionReferenceTarget::ValueType(STD_INTEGER_TYPE_ID),
        DefinitionReferenceKind::NamedType,
        references[0].source_origin(),
    ));
    fails(&StandardExecutable::new(revision.function(), revision.clone(), extra).unwrap());

    // Reordered references.
    let mut reordered = references.clone();
    reordered.swap(0, 1);
    fails(&StandardExecutable::new(revision.function(), revision.clone(), reordered).unwrap());

    // Wrong reference kind.
    let mut wrong_kind = references.clone();
    wrong_kind[0] = DefinitionReference::new(
        wrong_kind[0].source_function(),
        wrong_kind[0].source_revision(),
        wrong_kind[0].ordinal(),
        wrong_kind[0].target(),
        DefinitionReferenceKind::FunctionCall,
        wrong_kind[0].source_origin(),
    );
    fails(&StandardExecutable::new(revision.function(), revision.clone(), wrong_kind).unwrap());

    // Wrong reference target.
    let mut wrong_target = references.clone();
    wrong_target[1] = DefinitionReference::new(
        wrong_target[1].source_function(),
        wrong_target[1].source_revision(),
        wrong_target[1].ordinal(),
        DefinitionReferenceTarget::ValueType(TypeId::from_bytes([0x77; 16])),
        wrong_target[1].kind(),
        wrong_target[1].source_origin(),
    );
    fails(&StandardExecutable::new(revision.function(), revision.clone(), wrong_target).unwrap());

    // Wrong reference origin.
    let mut wrong_origin = references.clone();
    wrong_origin[2] = DefinitionReference::new(
        wrong_origin[2].source_function(),
        wrong_origin[2].source_revision(),
        wrong_origin[2].ordinal(),
        wrong_origin[2].target(),
        wrong_origin[2].kind(),
        SourceOrigin::new(STD_INVOKE_SOURCE_UNIT_ID, 0, 1).unwrap(),
    );
    fails(&StandardExecutable::new(revision.function(), revision.clone(), wrong_origin).unwrap());
}

/// The exact retained ADR 0058 `std/output.orna` source: the two output
/// schema declarations, the two opaque output value type declarations,
/// and their two qualified exports.
pub(super) const STANDARD_V3_OUTPUT_SOURCE: &str = "CREATE SCHEMA std.terminal;\nCREATE SCHEMA std.io;\n\nCREATE TYPE std.terminal.Document AS VALUE OPAQUE\n    KERNEL CONTRACT 'orna.std.value.terminal-document@1'\n    IMMUTABLE\n    TRANSIENT;\n\nEXPORT TYPE std.terminal.Document AS std.Document;\n\nCREATE TYPE std.io.ByteStream AS VALUE OPAQUE\n    KERNEL CONTRACT 'orna.std.value.byte-stream@1'\n    IMMUTABLE\n    TRANSIENT;\n\nEXPORT TYPE std.io.ByteStream AS std.ByteStream;";

fn standard_v3_catalogue(with_invoke: bool) -> CatalogueSnapshot {
    let catalogue = standard_v2_catalogue(with_invoke);
    let mut schemas = catalogue.schemas().to_vec();
    schemas.push(SchemaDefinition::new(
        STD_TERMINAL_SCHEMA_ID,
        QualifiedSemanticName::new(["std", "terminal"]).unwrap(),
    ));
    schemas.push(SchemaDefinition::new(
        STD_IO_SCHEMA_ID,
        QualifiedSemanticName::new(["std", "io"]).unwrap(),
    ));
    let mut value_types = catalogue.value_types().to_vec();
    value_types.push(ValueTypeDefinition::opaque(
        STD_TERMINAL_DOCUMENT_TYPE_ID,
        QualifiedSemanticName::new(["std", "terminal", "document"]).unwrap(),
        "orna.std.value.terminal-document@1",
    ));
    value_types.push(ValueTypeDefinition::opaque(
        STD_IO_BYTE_STREAM_TYPE_ID,
        QualifiedSemanticName::new(["std", "io", "bytestream"]).unwrap(),
        "orna.std.value.byte-stream@1",
    ));
    let mut type_bindings = catalogue.type_bindings().to_vec();
    type_bindings.push(
        TypeBinding::qualified(
            QualifiedSemanticName::new(["std", "document"]).unwrap(),
            STD_TERMINAL_DOCUMENT_TYPE_ID,
        )
        .unwrap(),
    );
    type_bindings.push(
        TypeBinding::qualified(
            QualifiedSemanticName::new(["std", "bytestream"]).unwrap(),
            STD_IO_BYTE_STREAM_TYPE_ID,
        )
        .unwrap(),
    );
    CatalogueSnapshot::new_with_functions_and_types(
        CatalogueRevisionId::from_bytes([0x21; 16]),
        schemas,
        vec![],
        value_types,
        type_bindings,
        catalogue.functions().to_vec(),
    )
    .unwrap()
}

fn standard_v3_catalogue_with_output_value_type(
    index: usize,
    definition: ValueTypeDefinition,
) -> CatalogueSnapshot {
    let catalogue = standard_v3_catalogue(true);
    let mut value_types = catalogue.value_types().to_vec();
    value_types[index] = definition;
    CatalogueSnapshot::new_with_functions_and_types(
        catalogue.revision(),
        catalogue.schemas().to_vec(),
        vec![],
        value_types,
        catalogue.type_bindings().to_vec(),
        catalogue.functions().to_vec(),
    )
    .unwrap()
}

fn standard_v3_units() -> (StoredSourceUnit, StoredSourceUnit, StoredSourceUnit) {
    (
        stored_v2_unit(
            STD_TYPES_SOURCE_UNIT_ID,
            0,
            "std/types.orna",
            STANDARD_V2_TYPES_SOURCE,
        ),
        stored_v2_unit(
            STD_INVOKE_SOURCE_UNIT_ID,
            1,
            "std/invoke.orna",
            STD_INVOKE_SOURCE,
        ),
        stored_v2_unit(
            STD_OUTPUT_SOURCE_UNIT_ID,
            2,
            "std/output.orna",
            STANDARD_V3_OUTPUT_SOURCE,
        ),
    )
}

pub(super) fn standard_v3_output_origins(
    catalogue: &CatalogueSnapshot,
    source: &str,
) -> Vec<DefinitionOrigin> {
    let report =
        parse_bundle(&SourceBundle::new([SourceUnit::new("std/output.orna", source)]).unwrap());
    assert!(report.diagnostics().is_empty(), "{source}");
    let parsed = &report.units()[0];
    let origin = |identity: DefinitionIdentity, span: &SourceSpan| -> DefinitionOrigin {
        DefinitionOrigin::new(
            identity,
            SourceOrigin::new(
                STD_OUTPUT_SOURCE_UNIT_ID,
                u32::try_from(span.start).unwrap(),
                u32::try_from(span.end).unwrap(),
            )
            .unwrap(),
        )
    };
    let document_binding = catalogue.type_binding_by_name(&TypeLookupName::qualified(
        QualifiedSemanticName::new(["std", "document"]).unwrap(),
    ));
    let bytestream_binding = catalogue.type_binding_by_name(&TypeLookupName::qualified(
        QualifiedSemanticName::new(["std", "bytestream"]).unwrap(),
    ));
    let mut origins = Vec::with_capacity(6);
    if let Some(schema) = parsed.parsed().schemas().first() {
        origins.push(origin(
            DefinitionIdentity::Schema(STD_TERMINAL_SCHEMA_ID),
            &schema.span,
        ));
    }
    if let Some(schema) = parsed.parsed().schemas().get(1) {
        origins.push(origin(
            DefinitionIdentity::Schema(STD_IO_SCHEMA_ID),
            &schema.span,
        ));
    }
    if let Some(value_type) = parsed.parsed().opaque_value_types().first() {
        origins.push(origin(
            DefinitionIdentity::ValueType(STD_TERMINAL_DOCUMENT_TYPE_ID),
            &value_type.span,
        ));
    }
    if let (Some(binding), Some(export)) =
        (document_binding, parsed.parsed().type_exports().first())
    {
        origins.push(origin(
            DefinitionIdentity::TypeBinding(binding.id()),
            &export.span,
        ));
    }
    if let Some(value_type) = parsed.parsed().opaque_value_types().get(1) {
        origins.push(origin(
            DefinitionIdentity::ValueType(STD_IO_BYTE_STREAM_TYPE_ID),
            &value_type.span,
        ));
    }
    if let (Some(binding), Some(export)) =
        (bytestream_binding, parsed.parsed().type_exports().get(1))
    {
        origins.push(origin(
            DefinitionIdentity::TypeBinding(binding.id()),
            &export.span,
        ));
    }
    origins
}

fn standard_v3_parts() -> (
    Vec<StoredSourceUnit>,
    CatalogueSnapshot,
    Vec<DefinitionOrigin>,
    Vec<StandardExecutable>,
) {
    let (types_unit, invoke_unit, output_unit) = standard_v3_units();
    let catalogue = standard_v3_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    origins.extend(standard_v3_output_origins(
        &catalogue,
        STANDARD_V3_OUTPUT_SOURCE,
    ));
    let executable = standard_v2_executable(&catalogue, &origins);
    (
        vec![types_unit, invoke_unit, output_unit],
        catalogue,
        origins,
        vec![executable],
    )
}

fn standard_v3_source(units: Vec<StoredSourceUnit>) -> StoredSourceRevision {
    let bundle_hash = source_bundle_digest(&units).unwrap();
    StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x51; 16]),
        SourceRevisionId::from_bytes([0x52; 16]),
        Some(SourceRevisionId::from_bytes([0x53; 16])),
        units,
        bundle_hash,
        source_revision_record_digest(
            SourceBundleId::from_bytes([0x51; 16]),
            Some(SourceRevisionId::from_bytes([0x53; 16])),
            bundle_hash,
        )
        .unwrap(),
    )
    .unwrap()
}

/// Runs the V3 source reconcile directly on raw stored facts, without the
/// separate digest-verification gate.
fn check_v3_parts(
    units: Vec<StoredSourceUnit>,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    check_standard_library_source_v3_parts(
        &standard_v3_source(units),
        catalogue,
        origins,
        executables,
    )
}

fn build_standard_v3_snapshot(
    units: Vec<StoredSourceUnit>,
    catalogue: CatalogueSnapshot,
    origins: Vec<DefinitionOrigin>,
    executables: Vec<StandardExecutable>,
    digest: Sha256Digest,
) -> StandardLibrarySnapshot {
    StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V3_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        standard_v3_source(units),
        "orna.language/1",
        catalogue,
        executables,
        origins,
        digest,
    )
    .unwrap()
}

/// The compiled canonical V3 standard-library digest for the exact test
/// inputs (`STANDARD_V2_TYPES_SOURCE`, `STD_INVOKE_SOURCE`,
/// `STANDARD_V3_OUTPUT_SOURCE`, the fixed identities, catalogue,
/// executable, and origins). Computed by the canonical encoder.
const STANDARD_V3_CANONICAL_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    190, 191, 32, 251, 204, 169, 87, 210, 50, 82, 209, 87, 203, 106, 51, 38, 191, 112, 175, 46, 92,
    50, 161, 93, 72, 2, 203, 116, 173, 102, 221, 131,
]);

fn verified_standard_v3_snapshot() -> VerifiedStandardLibrarySnapshot {
    let (units, catalogue, origins, executables) = standard_v3_parts();
    verify_standard_library_v2_snapshot(build_standard_v3_snapshot(
        units,
        catalogue,
        origins,
        executables,
        STANDARD_V3_CANONICAL_DIGEST,
    ))
    .unwrap()
}

#[test]
fn reconciles_the_exact_v3_standard_output_bundle() {
    let verified = verified_standard_v3_snapshot();
    assert_eq!(verified.revision(), STANDARD_LIBRARY_V3_REVISION_ID);
    assert_eq!(
        verified.digest_version(),
        StandardLibraryDigestVersion::Version2
    );
    let checked = check_standard_library_source(&verified).unwrap();

    assert_eq!(checked.schemas().len(), 2);
    assert_eq!(checked.value_types().len(), 1);
    assert_eq!(checked.type_bindings().len(), 2);

    let executable = checked.checked_executable().unwrap();
    assert_eq!(executable.parameter_ids(), &[STD_INVOKE_ECHO_PARAMETER_ID]);
    assert_eq!(executable.function_id(), STD_INVOKE_ECHO_FUNCTION_ID);
    assert_eq!(
        executable.revision_id(),
        STD_INVOKE_ECHO_FUNCTION_REVISION_ID
    );
    assert_eq!(
        executable.revision_number(),
        STD_INVOKE_ECHO_REVISION_NUMBER
    );
    assert_eq!(
        executable.semantic_hash_version(),
        FunctionSemanticHashVersion::Version2
    );
    assert_eq!(executable.language_version(), "orna.language/1");
    assert_eq!(executable.references().len(), 3);

    let stored = &verified.executables()[0];
    assert_eq!(executable.artifact(), stored.revision().artifact());
    assert_eq!(executable.references(), stored.references());
    assert_eq!(
        executable.declaration_origin(),
        stored.revision().declaration_origin()
    );
    assert_eq!(
        executable.declaration_content_hash(),
        stored.revision().declaration_content_hash()
    );
    assert_eq!(
        executable.semantic_hash(),
        stored.revision().semantic_hash()
    );

    // The retained output unit carries exactly the six output origins at
    // the exact declaration byte ranges.
    let output_origins = verified
        .origins()
        .iter()
        .filter(|origin| origin.source().source_unit() == STD_OUTPUT_SOURCE_UNIT_ID)
        .collect::<Vec<_>>();
    assert_eq!(output_origins.len(), 6);
    let document_origin = output_origins
        .iter()
        .find(|origin| {
            origin.identity() == DefinitionIdentity::ValueType(STD_TERMINAL_DOCUMENT_TYPE_ID)
        })
        .unwrap();
    assert_eq!(
        &STANDARD_V3_OUTPUT_SOURCE[document_origin.source().byte_start() as usize
            ..document_origin.source().byte_end() as usize],
        "CREATE TYPE std.terminal.Document AS VALUE OPAQUE\n    KERNEL CONTRACT 'orna.std.value.terminal-document@1'\n    IMMUTABLE\n    TRANSIENT;"
    );
    assert_eq!(verified.origins().len(), 14);
}

#[test]
fn rejects_a_v3_bundle_with_the_wrong_unit_identity_order_or_path() {
    let (types_unit, invoke_unit, output_unit) = standard_v3_units();
    let catalogue = standard_v3_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    origins.extend(standard_v3_output_origins(
        &catalogue,
        STANDARD_V3_OUTPUT_SOURCE,
    ));
    let executable = standard_v2_executable(&catalogue, &origins);
    let rejects = |units: Vec<StoredSourceUnit>, label: &str| {
        let error = check_v3_parts(
            units,
            &catalogue,
            &origins,
            std::slice::from_ref(&executable),
        )
        .unwrap_err();
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "{label}: unexpected rejection: {error}"
        );
    };

    rejects(
        vec![
            stored_v2_unit(
                SourceUnitId::from_bytes([0x77; 16]),
                0,
                "std/types.orna",
                STANDARD_V2_TYPES_SOURCE,
            ),
            invoke_unit.clone(),
            output_unit.clone(),
        ],
        "wrong types unit identity",
    );
    rejects(
        vec![
            types_unit.clone(),
            stored_v2_unit(
                STD_INVOKE_SOURCE_UNIT_ID,
                1,
                "std/invoke.orna",
                STD_INVOKE_SOURCE,
            ),
            stored_v2_unit(
                STD_OUTPUT_SOURCE_UNIT_ID,
                2,
                "std/out.orna",
                STANDARD_V3_OUTPUT_SOURCE,
            ),
        ],
        "wrong output unit path",
    );
    rejects(
        vec![
            types_unit,
            stored_v2_unit(
                STD_OUTPUT_SOURCE_UNIT_ID,
                1,
                "std/output.orna",
                STANDARD_V3_OUTPUT_SOURCE,
            ),
            stored_v2_unit(
                STD_INVOKE_SOURCE_UNIT_ID,
                2,
                "std/invoke.orna",
                STD_INVOKE_SOURCE,
            ),
        ],
        "swapped invoke and output units",
    );
}

#[test]
fn rejects_a_v3_bundle_with_a_missing_or_extra_unit() {
    let (types_unit, invoke_unit, output_unit) = standard_v3_units();
    let catalogue = standard_v3_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    origins.extend(standard_v3_output_origins(
        &catalogue,
        STANDARD_V3_OUTPUT_SOURCE,
    ));
    let executable = standard_v2_executable(&catalogue, &origins);

    let error = check_v3_parts(
        vec![types_unit.clone(), invoke_unit.clone()],
        &catalogue,
        &origins,
        std::slice::from_ref(&executable),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::SourceUnitCount { actual: 2 }
    ));

    let extra = stored_v2_unit(
        SourceUnitId::from_bytes([0x78; 16]),
        3,
        "std/extra.orna",
        "CREATE SCHEMA std.extra;",
    );
    let error = check_v3_parts(
        vec![types_unit.clone(), invoke_unit.clone(), output_unit, extra],
        &catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::SourceUnitCount { actual: 4 }
    ));
}

#[test]
fn rejects_every_output_unit_content_variation_closed() {
    let (types_unit, invoke_unit, _output_unit) = standard_v3_units();
    let catalogue = standard_v3_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut base_origins = standard_v2_types_origins(&catalogue, &parsed_types);
    base_origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let rejects_output = |source: &str, label: &str| {
        let output_unit = stored_v2_unit(STD_OUTPUT_SOURCE_UNIT_ID, 2, "std/output.orna", source);
        let mut origins = base_origins.clone();
        origins.extend(standard_v3_output_origins(&catalogue, source));
        let executable = standard_v2_executable(&catalogue, &origins);
        let error = check_v3_parts(
            vec![types_unit.clone(), invoke_unit.clone(), output_unit],
            &catalogue,
            &origins,
            &[executable],
        )
        .unwrap_err();
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "{label}: unexpected rejection: {error}"
        );
    };

    rejects_output(
        &STANDARD_V3_OUTPUT_SOURCE.replace("CREATE SCHEMA std.terminal;\n", ""),
        "missing terminal schema",
    );
    rejects_output(
        &STANDARD_V3_OUTPUT_SOURCE
            .replace("CREATE SCHEMA std.terminal;", "CREATE SCHEMA std.term;"),
        "wrong terminal schema name",
    );
    rejects_output(
            &STANDARD_V3_OUTPUT_SOURCE.replace(
                "CREATE TYPE std.terminal.Document AS VALUE OPAQUE\n    KERNEL CONTRACT 'orna.std.value.terminal-document@1'\n    IMMUTABLE\n    TRANSIENT;\n\n",
                "",
            ),
            "missing document type",
        );
    rejects_output(
        &STANDARD_V3_OUTPUT_SOURCE.replace(
            "CREATE TYPE std.terminal.Document",
            "CREATE TYPE std.terminal.Doc",
        ),
        "wrong document type name",
    );
    rejects_output(
        &STANDARD_V3_OUTPUT_SOURCE.replace(
            "'orna.std.value.terminal-document@1'",
            "'orna.std.value.terminal-document@2'",
        ),
        "wrong document contract",
    );
    rejects_output(
        &STANDARD_V3_OUTPUT_SOURCE.replace("AS std.Document;", "AS std.Doc;"),
        "wrong document export target",
    );
    rejects_output(
        &STANDARD_V3_OUTPUT_SOURCE.replace(
            "EXPORT TYPE std.terminal.Document",
            "EXPORT TYPE std.terminal.Doc",
        ),
        "wrong document export source",
    );
    rejects_output(
        &STANDARD_V3_OUTPUT_SOURCE.replace(
            "EXPORT TYPE std.terminal.Document AS std.Document;",
            "EXPORT TYPE std.terminal.Document TO PRELUDE AS Document;",
        ),
        "prelude document export",
    );
    rejects_output(
        &format!("{STANDARD_V3_OUTPUT_SOURCE}\nCREATE SCHEMA std.extra;"),
        "extra schema declaration",
    );
    rejects_output(
            &STANDARD_V3_OUTPUT_SOURCE.replace(
                "EXPORT TYPE std.io.ByteStream AS std.ByteStream;",
                "EXPORT TYPE std.io.ByteStream AS std.ByteStream;\n\nCREATE TYPE std.io.Extra AS VALUE OPAQUE\n    KERNEL CONTRACT 'orna.std.value.extra@1'\n    IMMUTABLE\n    TRANSIENT;",
            ),
            "extra opaque value type declaration",
        );
    rejects_output(
        &format!("{STANDARD_V3_OUTPUT_SOURCE}\nEXPORT TYPE std.io.ByteStream AS std.ByteStream;"),
        "extra export declaration",
    );
    rejects_output(
        &format!(
            "{STANDARD_V3_OUTPUT_SOURCE}\nCREATE TYPE std.extra.Value AS VALUE PRIMITIVE KERNEL CONTRACT 'extra@1' IMMUTABLE TRANSIENT;"
        ),
        "extra primitive value type declaration",
    );
}

#[test]
fn rejects_wrong_v3_output_catalogue_definitions_closed() {
    let (types_unit, invoke_unit, output_unit) = standard_v3_units();
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let rejects_catalogue = |catalogue: CatalogueSnapshot, label: &str| {
        let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
        origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
        origins.extend(standard_v3_output_origins(
            &catalogue,
            STANDARD_V3_OUTPUT_SOURCE,
        ));
        let executable = standard_v2_executable(&catalogue, &origins);
        let error = check_v3_parts(
            vec![types_unit.clone(), invoke_unit.clone(), output_unit.clone()],
            &catalogue,
            &origins,
            &[executable],
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                StandardLibraryCheckError::SourceMismatch
                    | StandardLibraryCheckError::MissingSchema
            ),
            "{label}: unexpected rejection: {error}"
        );
    };

    // Wrong document kernel contract at the fixed identity.
    rejects_catalogue(
        standard_v3_catalogue_with_output_value_type(
            1,
            ValueTypeDefinition::opaque(
                STD_TERMINAL_DOCUMENT_TYPE_ID,
                QualifiedSemanticName::new(["std", "terminal", "document"]).unwrap(),
                "orna.std.value.terminal-document@2",
            ),
        ),
        "wrong document contract",
    );
    // Document defined as a persistable primitive at the fixed identity,
    // not the opaque IMMUTABLE TRANSIENT output contract.
    rejects_catalogue(
        standard_v3_catalogue_with_output_value_type(
            1,
            ValueTypeDefinition::primitive(
                STD_TERMINAL_DOCUMENT_TYPE_ID,
                QualifiedSemanticName::new(["std", "terminal", "document"]).unwrap(),
                ValueTypeMutability::Immutable,
                ValueTypePersistence::Persistable,
                "orna.std.value.terminal-document@1",
            ),
        ),
        "wrong document mutability and persistence",
    );
    // ByteStream defined as a persistable primitive at the fixed identity.
    rejects_catalogue(
        standard_v3_catalogue_with_output_value_type(
            2,
            ValueTypeDefinition::primitive(
                STD_IO_BYTE_STREAM_TYPE_ID,
                QualifiedSemanticName::new(["std", "io", "bytestream"]).unwrap(),
                ValueTypeMutability::Immutable,
                ValueTypePersistence::Persistable,
                "orna.std.value.byte-stream@1",
            ),
        ),
        "wrong bytestream mutability and persistence",
    );
    // The terminal schema, document value type, and document binding are
    // missing from the catalogue.
    let catalogue = standard_v3_catalogue(true);
    let mut schemas = catalogue.schemas().to_vec();
    schemas.retain(|schema| schema.id() != STD_TERMINAL_SCHEMA_ID);
    let mut value_types = catalogue.value_types().to_vec();
    value_types.retain(|value_type| value_type.id() != STD_TERMINAL_DOCUMENT_TYPE_ID);
    let mut type_bindings = catalogue.type_bindings().to_vec();
    type_bindings.retain(|binding| binding.target() != STD_TERMINAL_DOCUMENT_TYPE_ID);
    let catalogue = CatalogueSnapshot::new_with_functions_and_types(
        catalogue.revision(),
        schemas,
        vec![],
        value_types,
        type_bindings,
        catalogue.functions().to_vec(),
    )
    .unwrap();
    rejects_catalogue(catalogue, "missing terminal schema and document");
    // The std.Document binding targets the wrong value type.
    let catalogue = standard_v3_catalogue(true);
    let mut type_bindings = catalogue.type_bindings().to_vec();
    let document_lookup =
        TypeLookupName::qualified(QualifiedSemanticName::new(["std", "document"]).unwrap());
    let document_index = type_bindings
        .iter()
        .position(|binding| binding.name() == &document_lookup)
        .unwrap();
    type_bindings[document_index] = TypeBinding::qualified(
        QualifiedSemanticName::new(["std", "document"]).unwrap(),
        STD_IO_BYTE_STREAM_TYPE_ID,
    )
    .unwrap();
    let catalogue = CatalogueSnapshot::new_with_functions_and_types(
        catalogue.revision(),
        catalogue.schemas().to_vec(),
        vec![],
        catalogue.value_types().to_vec(),
        type_bindings,
        catalogue.functions().to_vec(),
    )
    .unwrap();
    rejects_catalogue(catalogue, "wrong document binding target");
}

#[test]
fn rejects_swapped_output_declaration_order_closed() {
    // The retained origins bind each identity to its exact declaration
    // byte range; a source that swaps the two schema declarations shifts
    // those ranges and fails closed.
    let (types_unit, invoke_unit, _) = standard_v3_units();
    let catalogue = standard_v3_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    origins.extend(standard_v3_output_origins(
        &catalogue,
        STANDARD_V3_OUTPUT_SOURCE,
    ));
    let executable = standard_v2_executable(&catalogue, &origins);

    let swapped = STANDARD_V3_OUTPUT_SOURCE.replacen(
        "CREATE SCHEMA std.terminal;\nCREATE SCHEMA std.io;\n",
        "CREATE SCHEMA std.io;\nCREATE SCHEMA std.terminal;\n",
        1,
    );
    assert_ne!(swapped, STANDARD_V3_OUTPUT_SOURCE);
    let output_unit = stored_v2_unit(STD_OUTPUT_SOURCE_UNIT_ID, 2, "std/output.orna", &swapped);
    let error = check_v3_parts(
        vec![types_unit, invoke_unit, output_unit],
        &catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(
        matches!(error, StandardLibraryCheckError::SourceMismatch),
        "unexpected rejection: {error}"
    );
}

#[test]
fn rejects_wrong_v3_output_origin_ranges_closed() {
    let (types_unit, invoke_unit, output_unit) = standard_v3_units();
    let catalogue = standard_v3_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let assert_rejected = |error: StandardLibraryCheckError| {
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "unexpected rejection: {error}"
        );
    };
    let document_lookup =
        TypeLookupName::qualified(QualifiedSemanticName::new(["std", "document"]).unwrap());
    let document_binding_id = catalogue
        .type_binding_by_name(&document_lookup)
        .unwrap()
        .id();

    // Shifted document type origin range.
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let mut output_origins = standard_v3_output_origins(&catalogue, STANDARD_V3_OUTPUT_SOURCE);
    let document_origin = output_origins
        .iter_mut()
        .find(|origin| {
            origin.identity() == DefinitionIdentity::ValueType(STD_TERMINAL_DOCUMENT_TYPE_ID)
        })
        .unwrap();
    *document_origin = DefinitionOrigin::new(
        document_origin.identity(),
        SourceOrigin::new(
            STD_OUTPUT_SOURCE_UNIT_ID,
            document_origin.source().byte_start() + 1,
            document_origin.source().byte_end(),
        )
        .unwrap(),
    );
    origins.extend(output_origins);
    let executable = standard_v2_executable(&catalogue, &origins);
    assert_rejected(
        check_v3_parts(
            vec![types_unit.clone(), invoke_unit.clone(), output_unit.clone()],
            &catalogue,
            &origins,
            &[executable],
        )
        .unwrap_err(),
    );

    // Missing document export origin.
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let mut output_origins = standard_v3_output_origins(&catalogue, STANDARD_V3_OUTPUT_SOURCE);
    output_origins
        .retain(|origin| origin.identity() != DefinitionIdentity::TypeBinding(document_binding_id));
    origins.extend(output_origins);
    let executable = standard_v2_executable(&catalogue, &origins);
    assert_rejected(
        check_v3_parts(
            vec![types_unit.clone(), invoke_unit.clone(), output_unit.clone()],
            &catalogue,
            &origins,
            &[executable],
        )
        .unwrap_err(),
    );

    // Duplicate output origin identity.
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let mut output_origins = standard_v3_output_origins(&catalogue, STANDARD_V3_OUTPUT_SOURCE);
    let schema_origin = output_origins
        .iter()
        .find(|origin| origin.identity() == DefinitionIdentity::Schema(STD_TERMINAL_SCHEMA_ID))
        .unwrap()
        .clone();
    output_origins.push(schema_origin);
    origins.extend(output_origins);
    let executable = standard_v2_executable(&catalogue, &origins);
    assert_rejected(
        check_v3_parts(
            vec![types_unit.clone(), invoke_unit.clone(), output_unit.clone()],
            &catalogue,
            &origins,
            &[executable],
        )
        .unwrap_err(),
    );

    // Output origin on a foreign source unit.
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let mut output_origins = standard_v3_output_origins(&catalogue, STANDARD_V3_OUTPUT_SOURCE);
    let schema_origin = output_origins
        .iter_mut()
        .find(|origin| origin.identity() == DefinitionIdentity::Schema(STD_IO_SCHEMA_ID))
        .unwrap();
    *schema_origin = DefinitionOrigin::new(
        schema_origin.identity(),
        SourceOrigin::new(
            SourceUnitId::from_bytes([0x99; 16]),
            schema_origin.source().byte_start(),
            schema_origin.source().byte_end(),
        )
        .unwrap(),
    );
    origins.extend(output_origins);
    let executable = standard_v2_executable(&catalogue, &origins);
    assert_rejected(
        check_v3_parts(
            vec![types_unit, invoke_unit, output_unit],
            &catalogue,
            &origins,
            &[executable],
        )
        .unwrap_err(),
    );
}

#[test]
fn rejects_a_byte_modified_output_unit_closed() {
    let (types_unit, invoke_unit, _) = standard_v3_units();
    let catalogue = standard_v3_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    origins.extend(standard_v3_output_origins(
        &catalogue,
        STANDARD_V3_OUTPUT_SOURCE,
    ));
    let executable = standard_v2_executable(&catalogue, &origins);

    // Whitespace-only modification: tokens identical, declaration byte
    // ranges shift, so the stored origins no longer agree with the
    // retained source.
    let modified = STANDARD_V3_OUTPUT_SOURCE.replacen(
        "CREATE SCHEMA std.terminal;",
        "CREATE  SCHEMA std.terminal;",
        1,
    );
    assert_ne!(modified, STANDARD_V3_OUTPUT_SOURCE);
    let output_unit = stored_v2_unit(STD_OUTPUT_SOURCE_UNIT_ID, 2, "std/output.orna", &modified);
    let error = check_v3_parts(
        vec![types_unit.clone(), invoke_unit.clone(), output_unit],
        &catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(
        matches!(error, StandardLibraryCheckError::SourceMismatch),
        "unexpected rejection: {error}"
    );

    // Semantic modification: the schema declaration itself is rejected.
    let modified =
        STANDARD_V3_OUTPUT_SOURCE.replacen("CREATE SCHEMA std.io;", "CREATE SCHEMA std.other;", 1);
    let output_unit = stored_v2_unit(STD_OUTPUT_SOURCE_UNIT_ID, 2, "std/output.orna", &modified);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    origins.extend(standard_v3_output_origins(&catalogue, &modified));
    let executable = standard_v2_executable(&catalogue, &origins);
    let error = check_v3_parts(
        vec![types_unit, invoke_unit, output_unit],
        &catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(
        matches!(error, StandardLibraryCheckError::SourceMismatch),
        "unexpected rejection: {error}"
    );
}

#[test]
fn rejects_a_byte_modified_invoke_unit_through_the_v3_path() {
    let (types_unit, _, output_unit) = standard_v3_units();
    let catalogue = standard_v3_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    origins.extend(standard_v3_output_origins(
        &catalogue,
        STANDARD_V3_OUTPUT_SOURCE,
    ));
    let executable = standard_v2_executable(&catalogue, &origins);

    // The invoke unit reconciles exactly as the V2 checker does: a
    // semantic modification fails closed through the echo checker.
    let modified = STD_INVOKE_SOURCE.replacen("p_value INTEGER", "p_value BIGINT", 1);
    let invoke_unit = stored_v2_unit(STD_INVOKE_SOURCE_UNIT_ID, 1, "std/invoke.orna", &modified);
    let error = check_v3_parts(
        vec![types_unit, invoke_unit, output_unit],
        &catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::UnexpectedParameterType
    ));
}

#[test]
fn rejects_a_wrong_stored_executable_through_the_v3_path() {
    let (types_unit, invoke_unit, output_unit) = standard_v3_units();
    let catalogue = standard_v3_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    origins.extend(standard_v3_output_origins(
        &catalogue,
        STANDARD_V3_OUTPUT_SOURCE,
    ));
    let executable = standard_v2_executable(&catalogue, &origins);

    // Rebuild the stored executable with a different revision identity.
    let wrong_revision = FunctionRevisionId::from_bytes([0x11; 16]);
    let revision = executable.revision().clone();
    let references = executable
        .references()
        .iter()
        .map(|reference| {
            DefinitionReference::new(
                reference.source_function(),
                wrong_revision,
                reference.ordinal(),
                reference.target(),
                reference.kind(),
                reference.source_origin(),
            )
        })
        .collect::<Vec<_>>();
    let revision = FunctionRevisionRecord::new(
        revision.function(),
        wrong_revision,
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        revision.semantic_hash(),
        revision.language_version(),
        revision.artifact().clone(),
    )
    .unwrap()
    .with_semantic_hash_version(revision.semantic_hash_version());
    let executable = StandardExecutable::new(revision.function(), revision, references).unwrap();

    let error = check_v3_parts(
        vec![types_unit, invoke_unit, output_unit],
        &catalogue,
        &origins,
        &[executable],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::ExecutableMismatch
    ));
}

/// The exact retained ADR 0062 `std/ui.orna` source: the single `std.ui`
/// schema declaration, the single opaque UI value type declaration, and
/// its single qualified export.
pub(super) const STANDARD_V4_UI_SOURCE: &str = "CREATE SCHEMA std.ui;\n\nCREATE TYPE std.ui.UI AS VALUE\n    OPAQUE\n    KERNEL CONTRACT 'orna.std.value.ui@1'\n    IMMUTABLE\n    TRANSIENT;\n\nEXPORT TYPE std.ui.UI AS std.UI;";

pub(super) fn standard_v4_catalogue(with_invoke: bool) -> CatalogueSnapshot {
    let catalogue = standard_v3_catalogue(with_invoke);
    let mut schemas = catalogue.schemas().to_vec();
    schemas.push(SchemaDefinition::new(
        STD_UI_SCHEMA_ID,
        QualifiedSemanticName::new(["std", "ui"]).unwrap(),
    ));
    let mut value_types = catalogue.value_types().to_vec();
    value_types.push(ValueTypeDefinition::opaque(
        STD_UI_TYPE_ID,
        QualifiedSemanticName::new(["std", "ui", "ui"]).unwrap(),
        STD_UI_CONTRACT,
    ));
    let mut type_bindings = catalogue.type_bindings().to_vec();
    type_bindings.push(
        TypeBinding::qualified(
            QualifiedSemanticName::new(["std", "ui"]).unwrap(),
            STD_UI_TYPE_ID,
        )
        .unwrap(),
    );
    CatalogueSnapshot::new_with_functions_and_types(
        catalogue.revision(),
        schemas,
        vec![],
        value_types,
        type_bindings,
        catalogue.functions().to_vec(),
    )
    .unwrap()
}

pub(super) fn standard_v4_catalogue_with_ui_value_type(
    index: usize,
    definition: ValueTypeDefinition,
) -> CatalogueSnapshot {
    let catalogue = standard_v4_catalogue(true);
    let mut value_types = catalogue.value_types().to_vec();
    value_types[index] = definition;
    CatalogueSnapshot::new_with_functions_and_types(
        catalogue.revision(),
        catalogue.schemas().to_vec(),
        vec![],
        value_types,
        catalogue.type_bindings().to_vec(),
        catalogue.functions().to_vec(),
    )
    .unwrap()
}

pub(super) fn standard_v4_units() -> (
    StoredSourceUnit,
    StoredSourceUnit,
    StoredSourceUnit,
    StoredSourceUnit,
) {
    let (types_unit, invoke_unit, output_unit) = standard_v3_units();
    (
        types_unit,
        invoke_unit,
        output_unit,
        stored_v2_unit(
            STD_UI_SOURCE_UNIT_ID,
            3,
            "std/ui.orna",
            STANDARD_V4_UI_SOURCE,
        ),
    )
}

pub(super) fn standard_v4_ui_origins(
    catalogue: &CatalogueSnapshot,
    source: &str,
) -> Vec<DefinitionOrigin> {
    let report =
        parse_bundle(&SourceBundle::new([SourceUnit::new("std/ui.orna", source)]).unwrap());
    assert!(report.diagnostics().is_empty(), "{source}");
    let parsed = &report.units()[0];
    let origin = |identity: DefinitionIdentity, span: &SourceSpan| -> DefinitionOrigin {
        DefinitionOrigin::new(
            identity,
            SourceOrigin::new(
                STD_UI_SOURCE_UNIT_ID,
                u32::try_from(span.start).unwrap(),
                u32::try_from(span.end).unwrap(),
            )
            .unwrap(),
        )
    };
    let ui_binding = catalogue.type_binding_by_name(&TypeLookupName::qualified(
        QualifiedSemanticName::new(["std", "ui"]).unwrap(),
    ));
    let mut origins = Vec::with_capacity(3);
    if let Some(schema) = parsed.parsed().schemas().first() {
        origins.push(origin(
            DefinitionIdentity::Schema(STD_UI_SCHEMA_ID),
            &schema.span,
        ));
    }
    if let Some(value_type) = parsed.parsed().opaque_value_types().first() {
        origins.push(origin(
            DefinitionIdentity::ValueType(STD_UI_TYPE_ID),
            &value_type.span,
        ));
    }
    if let (Some(binding), Some(export)) = (ui_binding, parsed.parsed().type_exports().first()) {
        origins.push(origin(
            DefinitionIdentity::TypeBinding(binding.id()),
            &export.span,
        ));
    }
    origins
}

fn standard_v4_parts() -> (
    Vec<StoredSourceUnit>,
    CatalogueSnapshot,
    Vec<DefinitionOrigin>,
    Vec<StandardExecutable>,
) {
    let (types_unit, invoke_unit, output_unit, ui_unit) = standard_v4_units();
    let catalogue = standard_v4_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    origins.extend(standard_v3_output_origins(
        &catalogue,
        STANDARD_V3_OUTPUT_SOURCE,
    ));
    origins.extend(standard_v4_ui_origins(&catalogue, STANDARD_V4_UI_SOURCE));
    let executable = standard_v2_executable(&catalogue, &origins);
    (
        vec![types_unit, invoke_unit, output_unit, ui_unit],
        catalogue,
        origins,
        vec![executable],
    )
}

fn standard_v4_source(units: Vec<StoredSourceUnit>) -> StoredSourceRevision {
    let bundle_hash = source_bundle_digest(&units).unwrap();
    StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x61; 16]),
        SourceRevisionId::from_bytes([0x62; 16]),
        Some(SourceRevisionId::from_bytes([0x63; 16])),
        units,
        bundle_hash,
        source_revision_record_digest(
            SourceBundleId::from_bytes([0x61; 16]),
            Some(SourceRevisionId::from_bytes([0x63; 16])),
            bundle_hash,
        )
        .unwrap(),
    )
    .unwrap()
}

/// Runs the V4 source reconcile directly on raw stored facts, without the
/// separate digest-verification gate.
pub(super) fn check_v4_parts(
    units: Vec<StoredSourceUnit>,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    check_standard_library_source_v4_parts(
        &standard_v4_source(units),
        catalogue,
        origins,
        executables,
    )
}

fn build_standard_v4_snapshot(
    units: Vec<StoredSourceUnit>,
    catalogue: CatalogueSnapshot,
    origins: Vec<DefinitionOrigin>,
    executables: Vec<StandardExecutable>,
    digest: Sha256Digest,
) -> StandardLibrarySnapshot {
    StandardLibrarySnapshot::new_with_executables(
        STANDARD_LIBRARY_V4_REVISION_ID,
        StandardLibraryDigestVersion::Version2,
        standard_v4_source(units),
        "orna.language/1",
        catalogue,
        executables,
        origins,
        digest,
    )
    .unwrap()
}

/// The compiled canonical V4 standard-library digest for the exact test
/// inputs (`STANDARD_V2_TYPES_SOURCE`, `STD_INVOKE_SOURCE`,
/// `STANDARD_V3_OUTPUT_SOURCE`, `STANDARD_V4_UI_SOURCE`, the fixed
/// identities, catalogue, executable, and origins). Computed by the
/// canonical encoder.
const STANDARD_V4_CANONICAL_DIGEST: Sha256Digest = Sha256Digest::from_bytes([
    0xc3, 0xc4, 0x05, 0x29, 0xba, 0x69, 0xe3, 0x4e, 0x6d, 0x44, 0x1a, 0x83, 0x86, 0x9f, 0x5a, 0x9e,
    0x30, 0xc8, 0x71, 0x4d, 0x20, 0x55, 0x06, 0xfa, 0xa0, 0x5c, 0xd3, 0x96, 0x47, 0x09, 0xb5, 0xfc,
]);

pub(super) fn verified_standard_v4_snapshot() -> VerifiedStandardLibrarySnapshot {
    let (units, catalogue, origins, executables) = standard_v4_parts();
    verify_standard_library_v2_snapshot(build_standard_v4_snapshot(
        units,
        catalogue,
        origins,
        executables,
        STANDARD_V4_CANONICAL_DIGEST,
    ))
    .unwrap()
}

/// Reconciles the exact retained V4 bundle (types, invoke, output, ui)
/// against the source-independent V4 catalogue and proves the ui unit
/// contributes its schema, opaque value type, and qualified export at the
/// exact declaration byte ranges.
#[test]
fn reconciles_the_exact_v4_standard_bundle_with_the_ui_unit() {
    let verified = verified_standard_v4_snapshot();
    assert_eq!(verified.revision(), STANDARD_LIBRARY_V4_REVISION_ID);
    assert_eq!(
        verified.digest_version(),
        StandardLibraryDigestVersion::Version2
    );
    let checked = check_standard_library_source(&verified).unwrap();

    // The types/invoke reconcile surfaces the V2 schema, value type, and
    // binding facts unchanged; the output and ui units are reconciled
    // closed without contributing to the families.
    assert_eq!(checked.schemas().len(), 2);
    assert_eq!(checked.value_types().len(), 1);
    assert_eq!(checked.type_bindings().len(), 2);

    let executable = checked.checked_executable().unwrap();
    assert_eq!(executable.function_id(), STD_INVOKE_ECHO_FUNCTION_ID);

    // The ui unit contributes exactly -one- additional schema, the opaque
    // std.ui.ui value type, and the std.UI qualified binding; all present
    // in the retained snapshot at the exact declaration byte ranges.
    let ui_schema_origin = verified
        .origins()
        .iter()
        .find(|origin| {
            origin.source().source_unit() == STD_UI_SOURCE_UNIT_ID
                && origin.identity() == DefinitionIdentity::Schema(STD_UI_SCHEMA_ID)
        })
        .unwrap();
    assert_eq!(
        &STANDARD_V4_UI_SOURCE[ui_schema_origin.source().byte_start() as usize
            ..ui_schema_origin.source().byte_end() as usize],
        "CREATE SCHEMA std.ui;"
    );
    let ui_type_origin = verified
        .origins()
        .iter()
        .find(|origin| {
            origin.source().source_unit() == STD_UI_SOURCE_UNIT_ID
                && origin.identity() == DefinitionIdentity::ValueType(STD_UI_TYPE_ID)
        })
        .unwrap();
    assert_eq!(
        &STANDARD_V4_UI_SOURCE[ui_type_origin.source().byte_start() as usize
            ..ui_type_origin.source().byte_end() as usize],
        "CREATE TYPE std.ui.UI AS VALUE\n    OPAQUE\n    KERNEL CONTRACT 'orna.std.value.ui@1'\n    IMMUTABLE\n    TRANSIENT;"
    );
    let ui_binding_origin = verified
        .origins()
        .iter()
        .find(|origin| {
            origin.source().source_unit() == STD_UI_SOURCE_UNIT_ID
                && matches!(origin.identity(), DefinitionIdentity::TypeBinding(_))
        })
        .unwrap();
    assert_eq!(
        &STANDARD_V4_UI_SOURCE[ui_binding_origin.source().byte_start() as usize
            ..ui_binding_origin.source().byte_end() as usize],
        "EXPORT TYPE std.ui.UI AS std.UI;"
    );
    assert_eq!(verified.origins().len(), 17);
}

#[test]
fn rejects_a_v4_bundle_with_the_wrong_ui_unit_identity_order_or_path() {
    let (types_unit, invoke_unit, output_unit, _ui_unit) = standard_v4_units();
    let catalogue = standard_v4_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    origins.extend(standard_v3_output_origins(
        &catalogue,
        STANDARD_V3_OUTPUT_SOURCE,
    ));
    origins.extend(standard_v4_ui_origins(&catalogue, STANDARD_V4_UI_SOURCE));
    let executable = standard_v2_executable(&catalogue, &origins);
    let rejects = |units: Vec<StoredSourceUnit>, label: &str| {
        let error = check_v4_parts(
            units,
            &catalogue,
            &origins,
            std::slice::from_ref(&executable),
        )
        .unwrap_err();
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "{label}: unexpected rejection: {error}"
        );
    };

    rejects(
        vec![
            types_unit.clone(),
            invoke_unit.clone(),
            output_unit.clone(),
            stored_v2_unit(
                SourceUnitId::from_bytes([0x79; 16]),
                3,
                "std/ui.orna",
                STANDARD_V4_UI_SOURCE,
            ),
        ],
        "wrong ui unit identity",
    );
    // The ui content placed in the output slot (ordinal 2) with the output
    // unit displaced to ordinal 3 keeps the ordinals in sequence so the
    // parts checker sees a ui unit whose identity/ordinal do not match.
    rejects(
        vec![
            types_unit.clone(),
            invoke_unit.clone(),
            stored_v2_unit(
                STD_UI_SOURCE_UNIT_ID,
                2,
                "std/ui.orna",
                STANDARD_V4_UI_SOURCE,
            ),
            stored_v2_unit(
                STD_OUTPUT_SOURCE_UNIT_ID,
                3,
                "std/output.orna",
                STANDARD_V3_OUTPUT_SOURCE,
            ),
        ],
        "ui unit at wrong ordinal",
    );
    rejects(
        vec![
            types_unit,
            invoke_unit,
            output_unit,
            stored_v2_unit(
                STD_UI_SOURCE_UNIT_ID,
                3,
                "std/display.orna",
                STANDARD_V4_UI_SOURCE,
            ),
        ],
        "wrong ui unit path",
    );
}

#[test]
fn rejects_a_v4_bundle_with_a_missing_or_extra_unit() {
    let (types_unit, invoke_unit, output_unit, ui_unit) = standard_v4_units();
    let catalogue = standard_v4_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    let executable = standard_v2_executable(&catalogue, &origins);

    let error = check_v4_parts(
        vec![types_unit.clone(), invoke_unit.clone(), output_unit.clone()],
        &catalogue,
        &origins,
        std::slice::from_ref(&executable),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::SourceUnitCount { actual: 3 }
    ));

    let extra = stored_v2_unit(
        SourceUnitId::from_bytes([0x78; 16]),
        4,
        "std/extra.orna",
        "CREATE SCHEMA std.extra;",
    );
    let full_origins = {
        let mut o = origins.clone();
        o.extend(standard_v3_output_origins(
            &catalogue,
            STANDARD_V3_OUTPUT_SOURCE,
        ));
        o.extend(standard_v4_ui_origins(&catalogue, STANDARD_V4_UI_SOURCE));
        o
    };
    let full_executable = standard_v2_executable(&catalogue, &full_origins);
    let error = check_v4_parts(
        vec![types_unit, invoke_unit, output_unit.clone(), ui_unit, extra],
        &catalogue,
        &full_origins,
        &[full_executable],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        StandardLibraryCheckError::SourceUnitCount { actual: 5 }
    ));
}

#[test]
fn rejects_every_ui_unit_content_variation_closed() {
    let (types_unit, invoke_unit, output_unit, _ui_unit) = standard_v4_units();
    let catalogue = standard_v4_catalogue(true);
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut base_origins = standard_v2_types_origins(&catalogue, &parsed_types);
    base_origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    base_origins.extend(standard_v3_output_origins(
        &catalogue,
        STANDARD_V3_OUTPUT_SOURCE,
    ));
    let rejects_ui = |source: &str, label: &str| {
        let ui_unit = stored_v2_unit(STD_UI_SOURCE_UNIT_ID, 3, "std/ui.orna", source);
        let mut origins = base_origins.clone();
        origins.extend(standard_v4_ui_origins(&catalogue, source));
        let executable = standard_v2_executable(&catalogue, &origins);
        let error = check_v4_parts(
            vec![
                types_unit.clone(),
                invoke_unit.clone(),
                output_unit.clone(),
                ui_unit,
            ],
            &catalogue,
            &origins,
            &[executable],
        )
        .unwrap_err();
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "{label}: unexpected rejection: {error}"
        );
    };

    rejects_ui(
        &STANDARD_V4_UI_SOURCE.replace("CREATE SCHEMA std.ui;", "CREATE SCHEMA std.ux;"),
        "wrong ui schema name",
    );
    rejects_ui(
        &STANDARD_V4_UI_SOURCE.replace(
            "CREATE TYPE std.ui.UI AS VALUE",
            "CREATE TYPE std.ui.Window AS VALUE",
        ),
        "wrong ui type local name",
    );
    rejects_ui(
        &STANDARD_V4_UI_SOURCE.replace(
            "KERNEL CONTRACT 'orna.std.value.ui@1'",
            "KERNEL CONTRACT 'orna.std.value.window@1'",
        ),
        "wrong ui kernel contract",
    );
    rejects_ui(
        &STANDARD_V4_UI_SOURCE.replace(
            "EXPORT TYPE std.ui.UI AS std.UI;",
            "EXPORT TYPE std.ui.UI AS std.Window;",
        ),
        "wrong ui export binding",
    );
}

const STANDARD_V5_JSON_SOURCE: &str = include_str!("../../../../../stdlib/std/json.orna");
const STANDARD_V6_ACTION_SOURCE: &str = include_str!("../../../../../stdlib/std/action.orna");

fn standard_v5_catalogue() -> CatalogueSnapshot {
    let base = standard_v4_catalogue(true);
    let mut schemas = base.schemas().to_vec();
    schemas.push(SchemaDefinition::new(
        STD_JSON_SCHEMA_ID,
        QualifiedSemanticName::new(["std", "json"]).unwrap(),
    ));
    let mut value_types = base.value_types().to_vec();
    value_types.push(ValueTypeDefinition::opaque(
        STD_JSON_VALUE_TYPE_ID,
        QualifiedSemanticName::new(["std", "json", "value"]).unwrap(),
        STD_JSON_CONTRACT,
    ));
    let mut type_bindings = base.type_bindings().to_vec();
    type_bindings.push(
        TypeBinding::qualified(
            QualifiedSemanticName::new(["std", "jsonvalue"]).unwrap(),
            STD_JSON_VALUE_TYPE_ID,
        )
        .unwrap(),
    );
    let mut functions = base.functions().to_vec();
    functions.push(FunctionDefinition::new(
        STD_JSON_ENCODE_FUNCTION_ID,
        QualifiedSemanticName::new(["std", "json", "encode"]).unwrap(),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            STD_JSON_ENCODE_PARAMETER_ID,
            "p_value",
            0,
            ResolvedType::Named(STD_JSON_VALUE_TYPE_ID),
            None,
        )],
        FunctionReturn::Single(ResolvedType::Named(STD_IO_BYTE_STREAM_TYPE_ID)),
        STD_JSON_ENCODE_FUNCTION_REVISION_ID,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    ));
    CatalogueSnapshot::new_with_functions_and_types(
        base.revision(),
        schemas,
        vec![],
        value_types,
        type_bindings,
        functions,
    )
    .unwrap()
}

fn standard_v5_json_origins(catalogue: &CatalogueSnapshot) -> Vec<DefinitionOrigin> {
    let report = parse_bundle(
        &SourceBundle::new([SourceUnit::new("std/json.orna", STANDARD_V5_JSON_SOURCE)]).unwrap(),
    );
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let parsed = &report.units()[0];
    let binding = catalogue
        .type_binding_by_name(&TypeLookupName::qualified(
            QualifiedSemanticName::new(["std", "jsonvalue"]).unwrap(),
        ))
        .unwrap();
    let origin = |identity: DefinitionIdentity, span: &SourceSpan| {
        DefinitionOrigin::new(
            identity,
            SourceOrigin::new(
                super::super::STD_JSON_SOURCE_UNIT_ID,
                u32::try_from(span.start).unwrap(),
                u32::try_from(span.end).unwrap(),
            )
            .unwrap(),
        )
    };
    let schema = &parsed.parsed().schemas()[0];
    let value_type = &parsed.parsed().opaque_value_types()[0];
    let export = &parsed.parsed().type_exports()[0];
    let function = &parsed.parsed().server_functions()[0];
    vec![
        origin(DefinitionIdentity::Schema(STD_JSON_SCHEMA_ID), &schema.span),
        origin(
            DefinitionIdentity::ValueType(STD_JSON_VALUE_TYPE_ID),
            &value_type.span,
        ),
        origin(DefinitionIdentity::TypeBinding(binding.id()), &export.span),
        origin(
            DefinitionIdentity::Function(STD_JSON_ENCODE_FUNCTION_ID),
            &function.span,
        ),
        origin(
            DefinitionIdentity::Parameter {
                owner: STD_JSON_ENCODE_FUNCTION_ID,
                parameter: STD_JSON_ENCODE_PARAMETER_ID,
            },
            &function.parameters[0].span,
        ),
    ]
}

fn standard_v5_parts() -> (
    Vec<StoredSourceUnit>,
    CatalogueSnapshot,
    Vec<DefinitionOrigin>,
    Vec<StandardExecutable>,
) {
    let (types_unit, invoke_unit, output_unit, ui_unit) = standard_v4_units();
    let catalogue = standard_v5_catalogue();
    let parsed_types = parsed_standard_unit(STANDARD_V2_TYPES_SOURCE);
    let mut origins = standard_v2_types_origins(&catalogue, &parsed_types);
    origins.extend(standard_v2_invoke_origins(STD_INVOKE_SOURCE));
    origins.extend(standard_v3_output_origins(
        &catalogue,
        STANDARD_V3_OUTPUT_SOURCE,
    ));
    origins.extend(standard_v4_ui_origins(&catalogue, STANDARD_V4_UI_SOURCE));
    let json_origins = standard_v5_json_origins(&catalogue);
    origins.extend(json_origins.iter().cloned());
    let json_unit = stored_v2_unit(
        super::super::STD_JSON_SOURCE_UNIT_ID,
        4,
        "std/json.orna",
        STANDARD_V5_JSON_SOURCE,
    );
    let json_function = parsed_standard_unit(STANDARD_V5_JSON_SOURCE)
        .parsed()
        .server_functions()[0]
        .clone();
    let json_executable =
        expected_standard_json_executable(&json_function, &catalogue, &json_origins, &json_unit)
            .unwrap();
    let executable = standard_v2_executable(&catalogue, &origins);
    (
        vec![types_unit, invoke_unit, output_unit, ui_unit, json_unit],
        catalogue,
        origins,
        vec![executable, json_executable],
    )
}

fn check_v5_parts(
    units: Vec<StoredSourceUnit>,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    check_standard_library_source_v5_parts(&units, catalogue, origins, executables)
}

fn standard_v6_catalogue() -> CatalogueSnapshot {
    let base = standard_v5_catalogue();
    let mut schemas = base.schemas().to_vec();
    schemas.push(SchemaDefinition::new(
        STD_ACTION_SCHEMA_ID,
        QualifiedSemanticName::new(["std", "action"]).unwrap(),
    ));
    let mut value_types = base.value_types().to_vec();
    value_types.push(ValueTypeDefinition::opaque(
        STD_ACTION_TYPE_ID,
        QualifiedSemanticName::new(["std", "action", "action"]).unwrap(),
        STD_ACTION_CONTRACT,
    ));
    let mut type_bindings = base.type_bindings().to_vec();
    type_bindings.push(
        TypeBinding::qualified(
            QualifiedSemanticName::new(["std", "action"]).unwrap(),
            STD_ACTION_TYPE_ID,
        )
        .unwrap(),
    );
    CatalogueSnapshot::new_with_functions_and_types(
        base.revision(),
        schemas,
        vec![],
        value_types,
        type_bindings,
        base.functions().to_vec(),
    )
    .unwrap()
}

fn standard_v6_action_origins(catalogue: &CatalogueSnapshot) -> Vec<DefinitionOrigin> {
    let report = parse_bundle(
        &SourceBundle::new([SourceUnit::new(
            "std/action.orna",
            STANDARD_V6_ACTION_SOURCE,
        )])
        .unwrap(),
    );
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let parsed = &report.units()[0];
    let binding = catalogue
        .type_binding_by_name(&TypeLookupName::qualified(
            QualifiedSemanticName::new(["std", "action"]).unwrap(),
        ))
        .unwrap();
    let origin = |identity: DefinitionIdentity, span: &SourceSpan| {
        DefinitionOrigin::new(
            identity,
            SourceOrigin::new(
                STD_ACTION_SOURCE_UNIT_ID,
                u32::try_from(span.start).unwrap(),
                u32::try_from(span.end).unwrap(),
            )
            .unwrap(),
        )
    };
    let schema = &parsed.parsed().schemas()[0];
    let value_type = &parsed.parsed().opaque_value_types()[0];
    let export = &parsed.parsed().type_exports()[0];
    vec![
        origin(
            DefinitionIdentity::Schema(STD_ACTION_SCHEMA_ID),
            &schema.span,
        ),
        origin(
            DefinitionIdentity::ValueType(STD_ACTION_TYPE_ID),
            &value_type.span,
        ),
        origin(DefinitionIdentity::TypeBinding(binding.id()), &export.span),
    ]
}

fn standard_v6_parts() -> (
    Vec<StoredSourceUnit>,
    CatalogueSnapshot,
    Vec<DefinitionOrigin>,
    Vec<StandardExecutable>,
) {
    let (v5_units, _, v5_origins, executables) = standard_v5_parts();
    let catalogue = standard_v6_catalogue();
    let mut origins = v5_origins;
    origins.extend(standard_v6_action_origins(&catalogue));
    let action_unit = stored_v2_unit(
        STD_ACTION_SOURCE_UNIT_ID,
        5,
        "std/action.orna",
        STANDARD_V6_ACTION_SOURCE,
    );
    (
        v5_units.into_iter().chain([action_unit]).collect(),
        catalogue,
        origins,
        executables,
    )
}

fn check_v6_parts(
    units: Vec<StoredSourceUnit>,
    catalogue: &CatalogueSnapshot,
    origins: &[DefinitionOrigin],
    executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    check_standard_library_source_v6_parts(&units, catalogue, origins, executables)
}

#[test]
fn rejects_v5_when_a_retained_v4_unit_identity_order_path_or_ordinal_is_tampered() {
    let (units, catalogue, origins, executables) = standard_v5_parts();
    assert!(
        check_v5_parts(units.clone(), &catalogue, &origins, &executables).is_ok(),
        "canonical V5 fixture must be accepted before tamper checks",
    );
    for (label, replacement) in [
        (
            "identity",
            stored_v2_unit(
                SourceUnitId::from_bytes([0x9c; 16]),
                2,
                "std/output.orna",
                STANDARD_V3_OUTPUT_SOURCE,
            ),
        ),
        (
            "path",
            stored_v2_unit(
                STD_OUTPUT_SOURCE_UNIT_ID,
                2,
                "std/other.orna",
                STANDARD_V3_OUTPUT_SOURCE,
            ),
        ),
        (
            "ordinal",
            stored_v2_unit(
                STD_OUTPUT_SOURCE_UNIT_ID,
                9,
                "std/output.orna",
                STANDARD_V3_OUTPUT_SOURCE,
            ),
        ),
    ] {
        let mut tampered = units.clone();
        tampered[2] = replacement;
        let error = check_v5_parts(tampered, &catalogue, &origins, &executables).unwrap_err();
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "{label}: {error}"
        );
    }

    assert!(
        check_v5_parts(units.clone(), &catalogue, &origins, &executables).is_ok(),
        "canonical V5 fixture must be accepted before order tamper",
    );
    let mut tampered = units;
    tampered[2] = stored_v2_unit(
        STD_UI_SOURCE_UNIT_ID,
        2,
        "std/ui.orna",
        STANDARD_V4_UI_SOURCE,
    );
    tampered[3] = stored_v2_unit(
        STD_OUTPUT_SOURCE_UNIT_ID,
        3,
        "std/output.orna",
        STANDARD_V3_OUTPUT_SOURCE,
    );
    let error = check_v5_parts(tampered, &catalogue, &origins, &executables).unwrap_err();
    assert!(
        matches!(error, StandardLibraryCheckError::SourceMismatch),
        "swapped retained V4 units: {error}"
    );
}

#[test]
fn rejects_v5_when_the_json_unit_declaration_or_identity_is_tampered() {
    let (units, catalogue, origins, executables) = standard_v5_parts();
    assert!(
        check_v5_parts(units.clone(), &catalogue, &origins, &executables).is_ok(),
        "canonical V5 fixture must be accepted before tamper checks",
    );
    let rejects_source = |source: &str, label: &str| {
        let mut tampered = units.clone();
        tampered[4] = stored_v2_unit(STD_JSON_SOURCE_UNIT_ID, 4, "std/json.orna", source);
        let error = check_v5_parts(tampered, &catalogue, &origins, &executables).unwrap_err();
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "{label}: {error}"
        );
    };

    rejects_source(
        &STANDARD_V5_JSON_SOURCE.replace("CREATE SCHEMA std.json;", "CREATE SCHEMA std.jason;"),
        "wrong JSON schema",
    );
    rejects_source(
        &STANDARD_V5_JSON_SOURCE.replace(
            "CREATE TYPE std.json.Value AS VALUE",
            "CREATE TYPE std.json.Token AS VALUE",
        ),
        "wrong JSON opaque type name",
    );
    rejects_source(
        &STANDARD_V5_JSON_SOURCE.replace("orna.std.value.json@1", "orna.std.value.token@1"),
        "wrong JSON kernel contract",
    );
    rejects_source(
        &STANDARD_V5_JSON_SOURCE.replace(
            "EXPORT TYPE std.json.Value AS std.JsonValue;",
            "EXPORT TYPE std.json.Value AS std.JsonToken;",
        ),
        "wrong JSON export",
    );
    rejects_source(
        &format!("-- tampered\n{STANDARD_V5_JSON_SOURCE}"),
        "changed JSON source content",
    );

    for (label, replacement) in [
        (
            "wrong JSON source-unit identity",
            stored_v2_unit(
                SourceUnitId::from_bytes([0x9a; 16]),
                4,
                "std/json.orna",
                STANDARD_V5_JSON_SOURCE,
            ),
        ),
        (
            "wrong JSON source-unit ordinal",
            stored_v2_unit(
                STD_JSON_SOURCE_UNIT_ID,
                6,
                "std/json.orna",
                STANDARD_V5_JSON_SOURCE,
            ),
        ),
        (
            "wrong JSON source-unit path",
            stored_v2_unit(
                STD_JSON_SOURCE_UNIT_ID,
                4,
                "std/document.orna",
                STANDARD_V5_JSON_SOURCE,
            ),
        ),
    ] {
        let mut tampered = units.clone();
        tampered[4] = replacement;
        let error = check_v5_parts(tampered, &catalogue, &origins, &executables).unwrap_err();
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "{label}: {error}"
        );
    }
}

#[test]
fn rejects_v6_when_a_retained_v4_unit_identity_order_path_or_ordinal_is_tampered() {
    let (units, catalogue, origins, executables) = standard_v6_parts();
    assert!(
        check_v6_parts(units.clone(), &catalogue, &origins, &executables).is_ok(),
        "canonical V6 fixture must be accepted before tamper checks",
    );
    for (label, replacement) in [
        (
            "identity",
            stored_v2_unit(
                SourceUnitId::from_bytes([0x9d; 16]),
                2,
                "std/output.orna",
                STANDARD_V3_OUTPUT_SOURCE,
            ),
        ),
        (
            "path",
            stored_v2_unit(
                STD_OUTPUT_SOURCE_UNIT_ID,
                2,
                "std/other.orna",
                STANDARD_V3_OUTPUT_SOURCE,
            ),
        ),
        (
            "ordinal",
            stored_v2_unit(
                STD_OUTPUT_SOURCE_UNIT_ID,
                9,
                "std/output.orna",
                STANDARD_V3_OUTPUT_SOURCE,
            ),
        ),
    ] {
        let mut tampered = units.clone();
        tampered[2] = replacement;
        let error = check_v6_parts(tampered, &catalogue, &origins, &executables).unwrap_err();
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "{label}: {error}"
        );
    }

    assert!(
        check_v6_parts(units.clone(), &catalogue, &origins, &executables).is_ok(),
        "canonical V6 fixture must be accepted before order tamper",
    );
    let mut tampered = units;
    tampered[2] = stored_v2_unit(
        STD_UI_SOURCE_UNIT_ID,
        2,
        "std/ui.orna",
        STANDARD_V4_UI_SOURCE,
    );
    tampered[3] = stored_v2_unit(
        STD_OUTPUT_SOURCE_UNIT_ID,
        3,
        "std/output.orna",
        STANDARD_V3_OUTPUT_SOURCE,
    );
    let error = check_v6_parts(tampered, &catalogue, &origins, &executables).unwrap_err();
    assert!(
        matches!(error, StandardLibraryCheckError::SourceMismatch),
        "swapped retained V4 units: {error}"
    );
}

#[test]
fn rejects_v6_when_the_action_unit_declaration_or_identity_is_tampered() {
    let (units, catalogue, origins, executables) = standard_v6_parts();
    assert!(
        check_v6_parts(units.clone(), &catalogue, &origins, &executables).is_ok(),
        "canonical V6 fixture must be accepted before tamper checks",
    );
    let rejects_source = |source: &str, label: &str| {
        let mut tampered = units.clone();
        tampered[5] = stored_v2_unit(STD_ACTION_SOURCE_UNIT_ID, 5, "std/action.orna", source);
        let error = check_v6_parts(tampered, &catalogue, &origins, &executables).unwrap_err();
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "{label}: {error}"
        );
    };

    rejects_source(
        &STANDARD_V6_ACTION_SOURCE.replace("CREATE SCHEMA std.action;", "CREATE SCHEMA std.acted;"),
        "wrong action schema",
    );
    rejects_source(
        &STANDARD_V6_ACTION_SOURCE.replace(
            "CREATE TYPE std.action.Action AS VALUE",
            "CREATE TYPE std.action.Command AS VALUE",
        ),
        "wrong action opaque type name",
    );
    rejects_source(
        &STANDARD_V6_ACTION_SOURCE.replace(
            "KERNEL CONTRACT 'orna.std.value.action@1'",
            "KERNEL CONTRACT 'orna.std.value.command@1'",
        ),
        "wrong action kernel contract",
    );
    rejects_source(
        &STANDARD_V6_ACTION_SOURCE.replace(
            "EXPORT TYPE std.action.Action AS std.Action;",
            "EXPORT TYPE std.action.Action AS std.Command;",
        ),
        "wrong action export",
    );
    rejects_source(
        &STANDARD_V6_ACTION_SOURCE.replace("OPAQUE", "PRIMITIVE"),
        "wrong action value kind",
    );
    rejects_source(
        &format!("-- tampered\n{STANDARD_V6_ACTION_SOURCE}"),
        "changed action source content",
    );

    for (label, replacement) in [
        (
            "wrong action source-unit identity",
            stored_v2_unit(
                SourceUnitId::from_bytes([0x9b; 16]),
                5,
                "std/action.orna",
                STANDARD_V6_ACTION_SOURCE,
            ),
        ),
        (
            "wrong action source-unit ordinal",
            stored_v2_unit(
                STD_ACTION_SOURCE_UNIT_ID,
                7,
                "std/action.orna",
                STANDARD_V6_ACTION_SOURCE,
            ),
        ),
        (
            "wrong action source-unit path",
            stored_v2_unit(
                STD_ACTION_SOURCE_UNIT_ID,
                5,
                "std/command.orna",
                STANDARD_V6_ACTION_SOURCE,
            ),
        ),
    ] {
        let mut tampered = units.clone();
        tampered[5] = replacement;
        let error = check_v6_parts(tampered, &catalogue, &origins, &executables).unwrap_err();
        assert!(
            matches!(error, StandardLibraryCheckError::SourceMismatch),
            "{label}: {error}"
        );
    }

    for (label, source, message) in [
        (
            "wrong action mutability",
            STANDARD_V6_ACTION_SOURCE.replace("IMMUTABLE", "MUTABLE"),
            "expected IMMUTABLE after opaque codec contract",
        ),
        (
            "wrong action persistence",
            STANDARD_V6_ACTION_SOURCE.replace("TRANSIENT", "PERSISTABLE"),
            "expected TRANSIENT after IMMUTABLE",
        ),
    ] {
        let mut tampered = units.clone();
        tampered[5] = stored_v2_unit(STD_ACTION_SOURCE_UNIT_ID, 5, "std/action.orna", &source);
        let error = check_v6_parts(tampered, &catalogue, &origins, &executables).unwrap_err();
        let StandardLibraryCheckError::Diagnostics { diagnostics } = error else {
            panic!("{label}: expected parser diagnostics");
        };
        assert_eq!(diagnostics.len(), 1, "{label}: {diagnostics:?}");
        assert_eq!(diagnostics[0].message(), message, "{label}");
    }
}
