use super::*;

mod presenters;

pub use presenters::{check_standard_json_encode, check_standard_terminal_present_table};

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

/// Checks retained standard source against its verified catalogue and origins.
/// Historical pre-1.0 standard revisions are rejected explicitly; this
/// boundary does not reconstruct their retired source bundles.
pub fn check_standard_library_source(
    snapshot: &VerifiedStandardLibrarySnapshot,
) -> Result<CheckedStandardLibrary, StandardLibraryCheckError> {
    if snapshot.digest_version() == StandardLibraryDigestVersion::Version1 {
        check_standard_library_source_v1(snapshot)
    } else {
        Err(StandardLibraryCheckError::SourceMismatch)
    }
}

/// Pre-1.0 bundle checkers remain as explicit rejection boundaries for
/// internal callers that still present historical source facts.
#[cfg(test)]
pub(super) fn check_standard_library_source_v2_parts(
    _source: &StoredSourceRevision,
    _catalogue: &CatalogueSnapshot,
    _origins: &[DefinitionOrigin],
    _executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    Err(StandardLibraryCheckError::SourceMismatch)
}

#[cfg(test)]
pub(super) fn check_standard_library_source_v3_parts(
    _source: &StoredSourceRevision,
    _catalogue: &CatalogueSnapshot,
    _origins: &[DefinitionOrigin],
    _executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    Err(StandardLibraryCheckError::SourceMismatch)
}

#[cfg(test)]
pub(super) fn check_standard_library_source_v4_parts(
    _source: &StoredSourceRevision,
    _catalogue: &CatalogueSnapshot,
    _origins: &[DefinitionOrigin],
    _executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    Err(StandardLibraryCheckError::SourceMismatch)
}

/// Rejects the retired V2 parameter-echo source contract.
pub fn check_standard_parameter_echo(
    _declaration: &ServerFunctionDeclaration,
    _catalogue: &CatalogueSnapshot,
    _origins: &[DefinitionOrigin],
    _integer_type_id: TypeId,
) -> Result<CheckedStandardParameterEcho, StandardLibraryCheckError> {
    Err(StandardLibraryCheckError::SourceMismatch)
}

#[cfg(test)]
pub(super) fn check_standard_library_source_v5_parts(
    _source_units: &[StoredSourceUnit],
    _catalogue: &CatalogueSnapshot,
    _origins: &[DefinitionOrigin],
    _executables: &[StandardExecutable],
) -> Result<(StandardSourceFamilies, CheckedStandardExecutable), StandardLibraryCheckError> {
    Err(StandardLibraryCheckError::SourceMismatch)
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

#[cfg(test)]
pub(super) fn reconcile_standard_executable(
    _stored: &StandardExecutable,
    _checked: &CheckedStandardExecutable,
) -> Result<(), StandardLibraryCheckError> {
    Err(StandardLibraryCheckError::SourceMismatch)
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
