use super::*;
use crate::{
    canonical_hash::{
        calculate_standard_library_digest_for_test, source_bundle_digest,
        source_revision_record_digest, source_unit_content_digest,
        verify_standard_library_snapshot,
    },
    catalogue::{
        EnumTypeDefinition, FieldDefinition, FunctionDefinition, FunctionDomain, FunctionReturn,
        FunctionReturnColumnDefinition, FunctionSecurity, FunctionTransaction, FunctionVolatility,
        ObjectTypeDefinition, ParameterDefinition, QualifiedSemanticName,
        RecordValueFieldDefinition, RecordValueTypeDefinition, SchemaDefinition, TypeBinding,
        ValueTypeDefinition, ValueTypeMutability, ValueTypePersistence,
    },
    types::{ResolvedType, StandardScalar, TypeDescriptor, TypeDescriptorKind},
};

mod record_values;
mod reserved_identities;

const fn id<const BYTE: u8>() -> [u8; 16] {
    [BYTE; 16]
}

const fn digest<const BYTE: u8>() -> Sha256Digest {
    Sha256Digest::from_bytes([BYTE; 32])
}

#[test]
fn canonical_hash_versions_convert_from_exact_supported_numbers() {
    assert_eq!(CatalogueHashVersion::Version1.to_u32(), 1);
    assert_eq!(CatalogueHashVersion::Version2.to_u32(), 2);
    assert_eq!(FunctionSemanticHashVersion::Version1.to_u32(), 1);
    assert_eq!(FunctionSemanticHashVersion::Version2.to_u32(), 2);
    assert_eq!(StandardLibraryDigestVersion::Version1.to_u32(), 1);
    assert_eq!(StandardLibraryDigestVersion::Version2.to_u32(), 2);
    assert_eq!(
        CatalogueHashVersion::try_from(1),
        Ok(CatalogueHashVersion::Version1)
    );
    assert_eq!(
        CatalogueHashVersion::try_from(2),
        Ok(CatalogueHashVersion::Version2)
    );
    assert_eq!(
        FunctionSemanticHashVersion::try_from(1),
        Ok(FunctionSemanticHashVersion::Version1)
    );
    assert_eq!(
        FunctionSemanticHashVersion::try_from(2),
        Ok(FunctionSemanticHashVersion::Version2)
    );
    assert_eq!(
        StandardLibraryDigestVersion::try_from(1),
        Ok(StandardLibraryDigestVersion::Version1)
    );
    assert_eq!(
        StandardLibraryDigestVersion::try_from(2),
        Ok(StandardLibraryDigestVersion::Version2)
    );

    for unsupported in [0, 3, u32::MAX] {
        assert!(CatalogueHashVersion::try_from(unsupported).is_err());
        assert!(FunctionSemanticHashVersion::try_from(unsupported).is_err());
        assert!(StandardLibraryDigestVersion::try_from(unsupported).is_err());
    }
}

#[test]
fn unsupported_hash_versions_retain_the_exact_number_and_explain_the_hash_contract() {
    let catalogue = CatalogueHashVersion::try_from(41).unwrap_err();
    assert_eq!(
        catalogue,
        HashVersionError::UnsupportedCatalogue { value: 41 }
    );
    assert_eq!(
        catalogue.to_string(),
        "unsupported catalogue hash version 41"
    );

    let function_semantic = FunctionSemanticHashVersion::try_from(42).unwrap_err();
    assert_eq!(
        function_semantic,
        HashVersionError::UnsupportedFunctionSemantic { value: 42 }
    );
    assert_eq!(
        function_semantic.to_string(),
        "unsupported function semantic hash version 42"
    );

    let standard_library = StandardLibraryDigestVersion::try_from(43).unwrap_err();
    assert_eq!(
        standard_library,
        HashVersionError::UnsupportedStandardLibraryDigest { value: 43 }
    );
    assert_eq!(
        standard_library.to_string(),
        "unsupported standard library digest version 43"
    );
}

fn source(parent: Option<SourceRevisionId>) -> StoredSourceRevision {
    StoredSourceRevision::new(
        SourceBundleId::from_bytes(id::<1>()),
        SourceRevisionId::from_bytes(id::<2>()),
        parent,
        vec![
            StoredSourceUnit::new(
                SourceUnitId::from_bytes(id::<3>()),
                0,
                "crm/schema.orna",
                "CREATE SCHEMA crm;\n",
                digest::<3>(),
            )
            .unwrap(),
            StoredSourceUnit::new(
                SourceUnitId::from_bytes(id::<4>()),
                1,
                "crm/functions.orna",
                "-- cafe\u{301}\nFUNCTION crm.lookup;\n",
                digest::<4>(),
            )
            .unwrap(),
        ],
        digest::<5>(),
        digest::<6>(),
    )
    .unwrap()
}

fn empty_catalogue() -> CatalogueSnapshot {
    CatalogueSnapshot::new(CatalogueRevisionId::from_bytes(id::<7>()), vec![], vec![]).unwrap()
}

fn active_for_flat_type_conversion(
    catalogue: CatalogueSnapshot,
    catalogue_hash_context: CatalogueHashContext,
) -> ActiveDatabaseRevision {
    let source = source(None);
    ActiveDatabaseRevision {
        pair: RevisionPair::new(source.id(), catalogue.revision()),
        source,
        catalogue,
        catalogue_hash: digest::<7>(),
        catalogue_hash_context,
        expressions: vec![],
        function_revisions: vec![],
        historical_function_revisions: vec![],
        origins: vec![],
        references: vec![],
    }
}

fn flat_type_application_catalogue() -> CatalogueSnapshot {
    CatalogueSnapshot::new_with_record_value_types(
        CatalogueRevisionId::from_bytes(id::<7>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<8>()),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![ObjectTypeDefinition::new(
            TypeId::from_bytes(id::<80>()),
            QualifiedSemanticName::new(["crm", "contact"]).unwrap(),
            vec![],
        )],
        vec![ValueTypeDefinition::opaque(
            TypeId::from_bytes(id::<81>()),
            QualifiedSemanticName::new(["crm", "token"]).unwrap(),
            "crm.token@1",
        )],
        vec![EnumTypeDefinition::new(
            TypeId::from_bytes(id::<82>()),
            QualifiedSemanticName::new(["crm", "stage"]).unwrap(),
            ["lead"],
        )],
        vec![RecordValueTypeDefinition::new(
            TypeId::from_bytes(id::<83>()),
            QualifiedSemanticName::new(["crm", "status"]).unwrap(),
            vec![
                RecordValueFieldDefinition::try_new_descriptor(
                    FieldId::from_bytes(id::<84>()),
                    "active",
                    0,
                    TypeDescriptor::named(TypeId::from_bytes(id::<71>())),
                )
                .unwrap(),
            ],
        )],
        vec![],
    )
    .unwrap()
}

fn flat_type_standard_context() -> CatalogueHashContext {
    let catalogue = CatalogueSnapshot::new_with_enum_types(
        CatalogueRevisionId::from_bytes(id::<72>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<73>()),
            QualifiedSemanticName::new(["std"]).unwrap(),
        )],
        vec![],
        vec![
            standard_boolean_definition(),
            ValueTypeDefinition::opaque(
                TypeId::from_bytes(id::<74>()),
                QualifiedSemanticName::new(["std", "token"]).unwrap(),
                "std.token@1",
            ),
        ],
        vec![EnumTypeDefinition::new(
            TypeId::from_bytes(id::<75>()),
            QualifiedSemanticName::new(["std", "mode"]).unwrap(),
            ["safe"],
        )],
        vec![],
    )
    .unwrap();
    let content = "standard flat type descriptor fixture";
    let unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes(id::<3>()),
        0,
        "std/types.orna",
        content,
        source_unit_content_digest(content).unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes(id::<1>()),
        SourceRevisionId::from_bytes(id::<2>()),
        None,
        vec![unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes(id::<1>()), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let source_unit = SourceUnitId::from_bytes(id::<3>());
    let origins = [
        DefinitionIdentity::Schema(SchemaId::from_bytes(id::<73>())),
        DefinitionIdentity::ValueType(TypeId::from_bytes(id::<71>())),
        DefinitionIdentity::ValueType(TypeId::from_bytes(id::<74>())),
        DefinitionIdentity::ValueType(TypeId::from_bytes(id::<75>())),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, identity)| {
        DefinitionOrigin::new(
            identity,
            SourceOrigin::new(source_unit, index as u32, index as u32 + 1).unwrap(),
        )
    })
    .collect::<Vec<_>>();
    let provisional = StandardLibrarySnapshot::new(
        StandardLibraryRevisionId::from_bytes(id::<74>()),
        StandardLibraryDigestVersion::Version1,
        source.clone(),
        "orna.language/1",
        catalogue.clone(),
        origins.clone(),
        digest::<75>(),
    )
    .unwrap();
    let digest = calculate_standard_library_digest_for_test(&provisional).unwrap();
    let standard = StandardLibrarySnapshot::new(
        provisional.revision(),
        provisional.digest_version(),
        source,
        provisional.language_version(),
        catalogue,
        origins,
        digest,
    )
    .unwrap();
    CatalogueHashContext::version_two(verify_standard_library_snapshot(standard).unwrap())
}

#[test]
fn active_revision_converts_only_catalogue_identified_flat_type_leaves() {
    let active = active_for_flat_type_conversion(
        flat_type_application_catalogue(),
        flat_type_standard_context(),
    );

    for type_id in [
        TypeId::from_bytes(id::<82>()),
        TypeId::from_bytes(id::<83>()),
        TypeId::from_bytes(id::<75>()),
    ] {
        assert_eq!(
            active
                .type_descriptor_for(ResolvedType::named(type_id))
                .unwrap()
                .kind(),
            TypeDescriptorKind::Named(type_id)
        );
    }
    for type_id in [
        TypeId::from_bytes(id::<71>()),
        TypeId::from_bytes(id::<74>()),
    ] {
        assert_eq!(
            active
                .type_descriptor_for(ResolvedType::value(type_id))
                .unwrap()
                .kind(),
            TypeDescriptorKind::Named(type_id)
        );
    }
    let object = TypeId::from_bytes(id::<80>());
    assert_eq!(
        active
            .type_descriptor_for(ResolvedType::reference(object))
            .unwrap()
            .kind(),
        TypeDescriptorKind::Reference(object)
    );
}

#[test]
fn active_revision_flat_type_conversion_rejects_every_missing_or_wrong_category() {
    let active = active_for_flat_type_conversion(
        flat_type_application_catalogue(),
        flat_type_standard_context(),
    );
    let missing = TypeId::from_bytes(id::<99>());

    assert_eq!(
        active.type_descriptor_for(ResolvedType::scalar(StandardScalar::Boolean)),
        Err(FlatTypeDescriptorError::LegacyScalar {
            scalar: StandardScalar::Boolean,
        })
    );
    assert_eq!(
        active.type_descriptor_for(ResolvedType::named(missing)),
        Err(FlatTypeDescriptorError::UnknownNamedType { id: missing })
    );
    assert_eq!(
        active.type_descriptor_for(ResolvedType::named(TypeId::from_bytes(id::<80>()))),
        Err(FlatTypeDescriptorError::NamedObjectType {
            id: TypeId::from_bytes(id::<80>()),
        })
    );
    assert_eq!(
        active.type_descriptor_for(ResolvedType::named(TypeId::from_bytes(id::<81>()))),
        Err(FlatTypeDescriptorError::NamedValueType {
            id: TypeId::from_bytes(id::<81>()),
        })
    );
    assert_eq!(
        active.type_descriptor_for(ResolvedType::named(TypeId::from_bytes(id::<71>()))),
        Err(FlatTypeDescriptorError::NamedValueType {
            id: TypeId::from_bytes(id::<71>()),
        })
    );
    assert_eq!(
        active.type_descriptor_for(ResolvedType::value(missing)),
        Err(FlatTypeDescriptorError::UnknownStandardValueType {
            value_type: missing,
        })
    );
    assert_eq!(
        active.type_descriptor_for(ResolvedType::value(TypeId::from_bytes(id::<81>()))),
        Err(FlatTypeDescriptorError::UnknownStandardValueType {
            value_type: TypeId::from_bytes(id::<81>()),
        })
    );
    assert_eq!(
        active.type_descriptor_for(ResolvedType::value(TypeId::from_bytes(id::<75>()))),
        Err(FlatTypeDescriptorError::UnknownStandardValueType {
            value_type: TypeId::from_bytes(id::<75>()),
        })
    );
    assert_eq!(
        active.type_descriptor_for(ResolvedType::reference(TypeId::from_bytes(id::<82>()))),
        Err(FlatTypeDescriptorError::ReferenceTargetNotObject {
            target: TypeId::from_bytes(id::<82>()),
        })
    );
}

#[test]
fn active_revision_flat_type_conversion_closes_version_one_and_colliding_identities() {
    let value_type = TypeId::from_bytes(id::<71>());
    let version_one = active_for_flat_type_conversion(
        flat_type_application_catalogue(),
        CatalogueHashContext::version_one(),
    );
    assert_eq!(
        version_one.type_descriptor_for(ResolvedType::value(value_type)),
        Err(FlatTypeDescriptorError::StandardLibraryUnavailable { value_type })
    );
    assert_eq!(
        version_one.type_descriptor_for(ResolvedType::scalar(StandardScalar::Boolean)),
        Err(FlatTypeDescriptorError::LegacyScalar {
            scalar: StandardScalar::Boolean,
        })
    );

    let collision = TypeId::from_bytes(id::<75>());
    let application = CatalogueSnapshot::new_with_enum_types(
        CatalogueRevisionId::from_bytes(id::<7>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<8>()),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![EnumTypeDefinition::new(
            collision,
            QualifiedSemanticName::new(["crm", "mode"]).unwrap(),
            ["open"],
        )],
        vec![],
    )
    .unwrap();
    let active = active_for_flat_type_conversion(application, flat_type_standard_context());
    assert_eq!(
        active.type_descriptor_for(ResolvedType::named(collision)),
        Err(FlatTypeDescriptorError::AmbiguousNamedType { id: collision })
    );
}

#[test]
fn flat_type_descriptor_errors_have_exact_actionable_messages_without_sources() {
    let type_id = TypeId::from_bytes(id::<99>());
    let cases = [
        (
            FlatTypeDescriptorError::LegacyScalar {
                scalar: StandardScalar::Boolean,
            },
            "legacy scalar type has no catalogue identity",
        ),
        (
            FlatTypeDescriptorError::AmbiguousNamedType { id: type_id },
            "resolved named type is present in both application and standard catalogues",
        ),
        (
            FlatTypeDescriptorError::UnknownNamedType { id: type_id },
            "resolved named type is absent from the active catalogue",
        ),
        (
            FlatTypeDescriptorError::NamedObjectType { id: type_id },
            "resolved named type is an object and requires REF",
        ),
        (
            FlatTypeDescriptorError::NamedValueType { id: type_id },
            "resolved named type is a value definition and requires a value identity",
        ),
        (
            FlatTypeDescriptorError::StandardLibraryUnavailable {
                value_type: type_id,
            },
            "the active database has no standard library for the resolved value type",
        ),
        (
            FlatTypeDescriptorError::UnknownStandardValueType {
                value_type: type_id,
            },
            "resolved value type is absent from the pinned standard library",
        ),
        (
            FlatTypeDescriptorError::ReferenceTargetNotObject { target: type_id },
            "resolved reference target is not an active application object",
        ),
    ];

    for (error, expected) in cases {
        assert_eq!(error.to_string(), expected);
        assert!(error.source().is_none());
    }
}

fn unchecked_standard_with_catalogue_revision(
    catalogue_revision: CatalogueRevisionId,
) -> VerifiedStandardLibrarySnapshot {
    VerifiedStandardLibrarySnapshot::new(StandardLibrarySnapshot {
        inner: Arc::new(StandardLibrarySnapshotData {
            revision: StandardLibraryRevisionId::from_bytes(id::<74>()),
            digest_version: StandardLibraryDigestVersion::Version1,
            source: source(None),
            language_version: "orna.language/1".to_owned(),
            catalogue: CatalogueSnapshot::new(catalogue_revision, vec![], vec![]).unwrap(),
            executables: vec![],
            origins: vec![],
            digest: digest::<75>(),
        }),
    })
}

fn function_catalogue(function_revision: FunctionRevisionId) -> CatalogueSnapshot {
    function_catalogue_with_objects(function_revision, vec![])
}

fn function_catalogue_v2(function_revision: FunctionRevisionId) -> CatalogueSnapshot {
    function_catalogue_with_resolved_type(
        function_revision,
        vec![],
        ResolvedType::value(TypeId::from_bytes(id::<71>())),
    )
}

fn function_catalogue_with_objects(
    function_revision: FunctionRevisionId,
    object_types: Vec<ObjectTypeDefinition>,
) -> CatalogueSnapshot {
    function_catalogue_with_resolved_type(
        function_revision,
        object_types,
        ResolvedType::scalar(StandardScalar::Boolean),
    )
}

fn function_catalogue_with_resolved_type(
    function_revision: FunctionRevisionId,
    object_types: Vec<ObjectTypeDefinition>,
    resolved_type: ResolvedType,
) -> CatalogueSnapshot {
    function_catalogue_with_identity(
        FunctionId::from_bytes(id::<9>()),
        function_revision,
        object_types,
        resolved_type,
    )
}

fn function_catalogue_with_identity(
    function_id: FunctionId,
    function_revision: FunctionRevisionId,
    object_types: Vec<ObjectTypeDefinition>,
    resolved_type: ResolvedType,
) -> CatalogueSnapshot {
    let schema = SchemaDefinition::new(
        SchemaId::from_bytes(id::<8>()),
        QualifiedSemanticName::new(["crm"]).unwrap(),
    );
    let function = FunctionDefinition::new(
        function_id,
        QualifiedSemanticName::new(["crm", "lookup"]).unwrap(),
        FunctionDomain::Server,
        vec![],
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "found",
            0,
            resolved_type,
        )]),
        function_revision,
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Stable,
    );
    CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes(id::<7>()),
        vec![schema],
        object_types,
        vec![function],
    )
    .unwrap()
}

fn function_definition_named(
    name: &[&str],
    function_id: FunctionId,
    function_revision: FunctionRevisionId,
    resolved_type: ResolvedType,
) -> FunctionDefinition {
    FunctionDefinition::new(
        function_id,
        QualifiedSemanticName::new(name.iter().copied()).unwrap(),
        FunctionDomain::Server,
        vec![],
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "found",
            0,
            resolved_type,
        )]),
        function_revision,
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Stable,
    )
}

fn function_catalogue_with_functions(functions: Vec<FunctionDefinition>) -> CatalogueSnapshot {
    assert!(
        !functions.is_empty(),
        "the catalogue requires at least one function"
    );
    let namespace = {
        let parts = functions[0].name().parts();
        &parts[..parts.len() - 1]
    };
    assert!(
        functions.iter().all(|function| {
            let parts = function.name().parts();
            parts.len() == namespace.len() + 1 && &parts[..namespace.len()] == namespace
        }),
        "all supplied functions must share the same namespace"
    );
    let schema = SchemaDefinition::new(
        SchemaId::from_bytes(id::<8>()),
        QualifiedSemanticName::new(namespace.iter().cloned()).unwrap(),
    );
    CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes(id::<7>()),
        vec![schema],
        vec![],
        functions,
    )
    .unwrap()
}

fn resolved_type_slots_catalogue(
    field_type: ResolvedType,
    parameter_type: ResolvedType,
    return_type: FunctionReturn,
) -> CatalogueSnapshot {
    let schema = SchemaDefinition::new(
        SchemaId::from_bytes(id::<8>()),
        QualifiedSemanticName::new(["crm"]).unwrap(),
    );
    let object_type = ObjectTypeDefinition::new(
        TypeId::from_bytes(id::<80>()),
        QualifiedSemanticName::new(["crm", "task"]).unwrap(),
        vec![FieldDefinition::new(
            FieldId::from_bytes(id::<81>()),
            "value",
            0,
            field_type,
            false,
            false,
            None,
            None,
        )],
    );
    let function = FunctionDefinition::new(
        FunctionId::from_bytes(id::<82>()),
        QualifiedSemanticName::new(["crm", "enabled"]).unwrap(),
        FunctionDomain::Client,
        vec![ParameterDefinition::new(
            ParameterId::from_bytes(id::<83>()),
            "input",
            0,
            parameter_type,
            None,
        )],
        return_type,
        FunctionRevisionId::from_bytes(id::<84>()),
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Immutable,
    );
    CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes(id::<7>()),
        vec![schema],
        vec![object_type],
        vec![function],
    )
    .unwrap()
}

fn catalogue_with_opaque_client_return(
    opaque: TypeId,
    domain: FunctionDomain,
    parameters: Vec<ParameterDefinition>,
    security: FunctionSecurity,
    volatility: FunctionVolatility,
) -> CatalogueSnapshot {
    let schema = SchemaDefinition::new(
        SchemaId::from_bytes(id::<8>()),
        QualifiedSemanticName::new(["crm"]).unwrap(),
    );
    let function = FunctionDefinition::new(
        FunctionId::from_bytes(id::<82>()),
        QualifiedSemanticName::new(["crm", "token"]).unwrap(),
        domain,
        parameters,
        FunctionReturn::Single(ResolvedType::value(opaque)),
        FunctionRevisionId::from_bytes(id::<84>()),
        security,
        None,
        volatility,
    );
    CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes(id::<7>()),
        vec![schema],
        vec![],
        vec![function],
    )
    .unwrap()
}

#[test]
fn standard_library_snapshot_rejects_a_parented_source_with_exact_context() {
    let revision = StandardLibraryRevisionId::from_bytes(id::<74>());
    let parent = SourceRevisionId::from_bytes(id::<75>());
    let standard_source = source(Some(parent));
    let source_id = standard_source.id();

    let error = StandardLibrarySnapshot::new(
        revision,
        StandardLibraryDigestVersion::Version1,
        standard_source,
        "orna.language/1",
        empty_catalogue(),
        vec![],
        digest::<76>(),
    )
    .unwrap_err();

    assert_eq!(
        error,
        RevisionInvariantError::StandardLibrarySourceHasParent {
            source: source_id,
            parent: Some(parent),
        }
    );
    assert_eq!(
        error.to_string(),
        "standard library source revision has a parent"
    );
}

#[test]
fn standard_library_snapshot_rejects_an_empty_language_version_with_exact_revision() {
    let revision = StandardLibraryRevisionId::from_bytes(id::<74>());

    let error = StandardLibrarySnapshot::new(
        revision,
        StandardLibraryDigestVersion::Version1,
        source(None),
        "",
        empty_catalogue(),
        vec![],
        digest::<76>(),
    )
    .unwrap_err();

    assert_eq!(
        error,
        RevisionInvariantError::EmptyStandardLibraryLanguageVersion { revision }
    );
    assert_eq!(
        error.to_string(),
        "standard library language version is empty"
    );
}

#[test]
fn rejects_the_offline_application_sentinel_from_a_standard_snapshot() {
    let revision = StandardLibraryRevisionId::from_bytes(id::<74>());
    let catalogue =
        CatalogueSnapshot::new(EMPTY_APPLICATION_CATALOGUE_REVISION_ID, vec![], vec![]).unwrap();

    let error = StandardLibrarySnapshot::new(
        revision,
        StandardLibraryDigestVersion::Version1,
        source(None),
        "orna.language/1",
        catalogue,
        vec![],
        digest::<75>(),
    )
    .unwrap_err();

    assert_eq!(
        error,
        RevisionInvariantError::ReservedOfflineCheckCatalogueRevision {
            revision: EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
            role: DurableCatalogueRevisionRole::ActiveOrRecoveredStandard,
        }
    );
    assert_eq!(
        error.to_string(),
        "the reserved offline-check catalogue identity cannot be used in a durable revision"
    );
    assert!(Error::source(&error).is_none());
}

#[test]
fn rejects_the_offline_application_sentinel_from_active_and_deployable_positions_in_order() {
    let regular_catalogue = CatalogueRevisionId::from_bytes(id::<7>());
    let expected_source = SourceRevisionId::from_bytes(id::<81>());
    let candidate_source = source(Some(expected_source));
    let assert_reserved = |error: RevisionInvariantError, role| {
        assert_eq!(
            error,
            RevisionInvariantError::ReservedOfflineCheckCatalogueRevision {
                revision: EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
                role,
            }
        );
        assert_eq!(
            error.to_string(),
            "the reserved offline-check catalogue identity cannot be used in a durable revision"
        );
        assert!(Error::source(&error).is_none());
    };

    let active_source = source(None);
    let active_error = ActiveDatabaseRevision::new(
        RevisionPair::new(active_source.id(), EMPTY_APPLICATION_CATALOGUE_REVISION_ID),
        active_source,
        CatalogueSnapshot::new(EMPTY_APPLICATION_CATALOGUE_REVISION_ID, vec![], vec![]).unwrap(),
        digest::<76>(),
        vec![],
        vec![],
        vec![],
        vec![],
    )
    .unwrap_err();
    assert_reserved(
        active_error,
        DurableCatalogueRevisionRole::ActiveOrRecoveredApplication,
    );

    let active_source = source(None);
    let app_before_standard_error = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(active_source.id(), EMPTY_APPLICATION_CATALOGUE_REVISION_ID),
            active_source,
            CatalogueSnapshot::new(EMPTY_APPLICATION_CATALOGUE_REVISION_ID, vec![], vec![])
                .unwrap(),
            digest::<76>(),
            ActiveRevisionContent::new(vec![], vec![], vec![], vec![]),
        ),
        CatalogueHashContext::version_two(unchecked_standard_with_catalogue_revision(
            EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
        )),
    )
    .unwrap_err();
    assert_reserved(
        app_before_standard_error,
        DurableCatalogueRevisionRole::ActiveOrRecoveredApplication,
    );

    let active_source = source(None);
    let standard_error = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(active_source.id(), regular_catalogue),
            active_source,
            CatalogueSnapshot::new(regular_catalogue, vec![], vec![]).unwrap(),
            digest::<76>(),
            ActiveRevisionContent::new(vec![], vec![], vec![], vec![]),
        ),
        CatalogueHashContext::version_two(unchecked_standard_with_catalogue_revision(
            EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
        )),
    )
    .unwrap_err();
    assert_reserved(
        standard_error,
        DurableCatalogueRevisionRole::ActiveOrRecoveredStandard,
    );

    let deployable = |expected_catalogue, parent_catalogue, candidate_catalogue| {
        DeployableRevision::new(
            RevisionPair::new(expected_source, expected_catalogue),
            candidate_source.clone(),
            parent_catalogue,
            CatalogueSnapshot::new(candidate_catalogue, vec![], vec![]).unwrap(),
            digest::<77>(),
            vec![],
            vec![],
            vec![],
            vec![],
        )
    };
    assert_reserved(
        deployable(
            EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
            EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
            EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
        )
        .unwrap_err(),
        DurableCatalogueRevisionRole::DeployableExpectedBase,
    );
    assert_reserved(
        deployable(
            regular_catalogue,
            EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
            EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
        )
        .unwrap_err(),
        DurableCatalogueRevisionRole::DeployableParent,
    );
    assert_reserved(
        deployable(
            regular_catalogue,
            regular_catalogue,
            EMPTY_APPLICATION_CATALOGUE_REVISION_ID,
        )
        .unwrap_err(),
        DurableCatalogueRevisionRole::DeployableCandidate,
    );
}

#[test]
fn standard_library_snapshot_rejects_object_and_function_definitions_with_exact_revision() {
    let revision = StandardLibraryRevisionId::from_bytes(id::<74>());
    let object_catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes(id::<7>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<8>()),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![ObjectTypeDefinition::new(
            TypeId::from_bytes(id::<12>()),
            QualifiedSemanticName::new(["crm", "contact"]).unwrap(),
            vec![],
        )],
    )
    .unwrap();
    assert_eq!(object_catalogue.object_types().len(), 1);
    assert!(object_catalogue.functions().is_empty());

    let function_catalogue = function_catalogue(FunctionRevisionId::from_bytes(id::<11>()));
    assert!(function_catalogue.object_types().is_empty());
    assert_eq!(function_catalogue.functions().len(), 1);

    for catalogue in [object_catalogue, function_catalogue] {
        let error = StandardLibrarySnapshot::new(
            revision,
            StandardLibraryDigestVersion::Version1,
            source(None),
            "orna.language/1",
            catalogue,
            vec![],
            digest::<76>(),
        )
        .unwrap_err();

        assert_eq!(
            error,
            RevisionInvariantError::UnsupportedStandardLibraryDefinition { revision }
        );
        assert_eq!(
            error.to_string(),
            "standard library catalogue contains an unsupported definition"
        );
    }
}

#[test]
fn version_two_standard_snapshot_requires_complete_ordered_executable_evidence() {
    let function = FunctionId::from_bytes(id::<90>());
    assert!(matches!(
        StandardExecutable::new(function, function_revision(), vec![]),
        Err(RevisionInvariantError::StandardExecutableFunctionMismatch { .. })
    ));
    let function_revision = FunctionRevisionId::from_bytes(id::<91>());
    let schema = SchemaDefinition::new(
        SchemaId::from_bytes(id::<92>()),
        QualifiedSemanticName::new(["std", "invoke"]).unwrap(),
    );
    let catalogue = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes(id::<93>()),
        vec![schema.clone()],
        vec![],
        vec![FunctionDefinition::new(
            function,
            QualifiedSemanticName::new(["std", "invoke", "echo"]).unwrap(),
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
            function_revision,
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )],
    )
    .unwrap();
    let snapshot_source = source(Some(SourceRevisionId::from_bytes(id::<94>())));
    let declaration = SourceOrigin::new(SourceUnitId::from_bytes(id::<4>()), 10, 29).unwrap();
    let revision = FunctionRevisionRecord::new(
        function,
        function_revision,
        1,
        declaration,
        digest::<95>(),
        digest::<96>(),
        "orna.language/1",
        artifact(),
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let executable = StandardExecutable::new(function, revision.clone(), vec![]).unwrap();
    let origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(schema.id()),
            SourceOrigin::new(SourceUnitId::from_bytes(id::<3>()), 0, 1).unwrap(),
        ),
        DefinitionOrigin::new(DefinitionIdentity::Function(function), declaration),
    ];

    let snapshot = StandardLibrarySnapshot::new_with_executables(
        StandardLibraryRevisionId::from_bytes(id::<97>()),
        StandardLibraryDigestVersion::Version2,
        snapshot_source.clone(),
        "orna.language/1",
        catalogue.clone(),
        vec![executable.clone()],
        origins.clone(),
        digest::<98>(),
    )
    .unwrap();
    assert_eq!(snapshot.executables(), [executable]);

    let lower_function = FunctionId::from_bytes(id::<89>());
    let lower_revision = FunctionRevisionId::from_bytes(id::<88>());
    let lower_definition = FunctionDefinition::new(
        lower_function,
        QualifiedSemanticName::new(["std", "invoke", "later"]).unwrap(),
        FunctionDomain::Server,
        vec![],
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
        lower_revision,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    let reordered_catalogue = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes(id::<93>()),
        vec![schema.clone()],
        vec![],
        vec![
            snapshot.catalogue().functions()[0].clone(),
            lower_definition,
        ],
    )
    .unwrap();
    let lower_executable = StandardExecutable::new(
        lower_function,
        FunctionRevisionRecord::new(
            lower_function,
            lower_revision,
            1,
            declaration,
            digest::<87>(),
            digest::<86>(),
            "orna.language/1",
            artifact(),
        )
        .unwrap()
        .with_semantic_hash_version(FunctionSemanticHashVersion::Version2),
        vec![],
    )
    .unwrap();
    let mut reordered_origins = snapshot.origins().to_vec();
    reordered_origins.push(DefinitionOrigin::new(
        DefinitionIdentity::Function(lower_function),
        declaration,
    ));
    assert!(matches!(
        StandardLibrarySnapshot::new_with_executables(
            StandardLibraryRevisionId::from_bytes(id::<97>()),
            StandardLibraryDigestVersion::Version2,
            source(Some(SourceRevisionId::from_bytes(id::<94>()))),
            "orna.language/1",
            reordered_catalogue,
            vec![snapshot.executables()[0].clone(), lower_executable],
            reordered_origins,
            digest::<98>(),
        ),
        Err(RevisionInvariantError::StandardExecutableCatalogueFunctionOrder { ordinal: 1, .. })
    ));

    assert!(matches!(
        StandardLibrarySnapshot::new_with_executables(
            StandardLibraryRevisionId::from_bytes(id::<97>()),
            StandardLibraryDigestVersion::Version2,
            source(None),
            "orna.language/1",
            catalogue.clone(),
            vec![],
            origins.clone(),
            digest::<98>(),
        ),
        Err(RevisionInvariantError::VersionTwoStandardLibrarySourceHasNoParent { .. })
    ));
    assert!(matches!(
        StandardLibrarySnapshot::new_with_executables(
            StandardLibraryRevisionId::from_bytes(id::<97>()),
            StandardLibraryDigestVersion::Version2,
            snapshot_source,
            "orna.language/1",
            catalogue,
            vec![],
            origins,
            digest::<98>(),
        ),
        Err(RevisionInvariantError::StandardExecutableSequenceLengthMismatch { .. })
    ));

    let version_one_executable = StandardExecutable::new(
        function,
        FunctionRevisionRecord::new(
            function,
            function_revision,
            1,
            declaration,
            digest::<95>(),
            digest::<96>(),
            "orna.language/1",
            artifact(),
        )
        .unwrap(),
        vec![],
    )
    .unwrap();
    assert!(matches!(
        StandardLibrarySnapshot::new_with_executables(
            StandardLibraryRevisionId::from_bytes(id::<97>()),
            StandardLibraryDigestVersion::Version2,
            source(Some(SourceRevisionId::from_bytes(id::<94>()))),
            "orna.language/1",
            snapshot.catalogue().clone(),
            vec![version_one_executable],
            snapshot.origins().to_vec(),
            digest::<98>(),
        ),
        Err(RevisionInvariantError::StandardExecutableSemanticHashVersionMismatch { .. })
    ));

    let out_of_order = StandardExecutable::new(
        function,
        revision.clone(),
        vec![DefinitionReference::new(
            function,
            function_revision,
            1,
            DefinitionReferenceTarget::Function(function),
            DefinitionReferenceKind::FunctionCall,
            declaration,
        )],
    )
    .unwrap();
    assert!(matches!(
        StandardLibrarySnapshot::new_with_executables(
            StandardLibraryRevisionId::from_bytes(id::<97>()),
            StandardLibraryDigestVersion::Version2,
            source(Some(SourceRevisionId::from_bytes(id::<94>()))),
            "orna.language/1",
            snapshot.catalogue().clone(),
            vec![out_of_order],
            snapshot.origins().to_vec(),
            digest::<98>(),
        ),
        Err(RevisionInvariantError::StandardExecutableReferenceOrdinalOutOfSequence { .. })
    ));

    let crossed_reference = StandardExecutable::new(
        function,
        revision,
        vec![DefinitionReference::new(
            FunctionId::from_bytes(id::<99>()),
            function_revision,
            0,
            DefinitionReferenceTarget::Function(function),
            DefinitionReferenceKind::FunctionCall,
            declaration,
        )],
    )
    .unwrap();
    assert!(matches!(
        StandardLibrarySnapshot::new_with_executables(
            StandardLibraryRevisionId::from_bytes(id::<97>()),
            StandardLibraryDigestVersion::Version2,
            source(Some(SourceRevisionId::from_bytes(id::<94>()))),
            "orna.language/1",
            snapshot.catalogue().clone(),
            vec![crossed_reference],
            snapshot.origins().to_vec(),
            digest::<98>(),
        ),
        Err(RevisionInvariantError::StandardExecutableReferenceOwnerMismatch { .. })
    ));
}

fn write_catalogue(function_revision: FunctionRevisionId) -> CatalogueSnapshot {
    let object_type = ObjectTypeDefinition::new(
        TypeId::from_bytes(id::<12>()),
        QualifiedSemanticName::new(["crm", "contact"]).unwrap(),
        vec![FieldDefinition::new(
            FieldId::from_bytes(id::<13>()),
            "active",
            0,
            ResolvedType::scalar(StandardScalar::Boolean),
            false,
            true,
            None,
            None,
        )],
    );
    function_catalogue_with_objects(function_revision, vec![object_type])
}

fn artifact() -> ExecutableArtifact {
    ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        "orna.server-plan",
        1,
        vec![1, 2, 3],
        digest::<10>(),
    )
    .unwrap()
}

fn function_revision() -> FunctionRevisionRecord {
    function_revision_fixture(
        FunctionId::from_bytes(id::<9>()),
        FunctionRevisionId::from_bytes(id::<11>()),
        digest::<11>(),
        digest::<12>(),
    )
}

fn function_revision_fixture(
    function: FunctionId,
    revision: FunctionRevisionId,
    declaration_content_hash: Sha256Digest,
    semantic_hash: Sha256Digest,
) -> FunctionRevisionRecord {
    FunctionRevisionRecord::new(
        function,
        revision,
        1,
        SourceOrigin::new(SourceUnitId::from_bytes(id::<4>()), 10, 29).unwrap(),
        declaration_content_hash,
        semantic_hash,
        "orna-1",
        artifact(),
    )
    .unwrap()
}

fn function_revision_v2() -> FunctionRevisionRecord {
    function_revision().with_semantic_hash_version(FunctionSemanticHashVersion::Version2)
}

fn resolved_slot_function_revision() -> FunctionRevisionRecord {
    FunctionRevisionRecord::new(
        FunctionId::from_bytes(id::<82>()),
        FunctionRevisionId::from_bytes(id::<84>()),
        1,
        SourceOrigin::new(SourceUnitId::from_bytes(id::<4>()), 10, 29).unwrap(),
        digest::<11>(),
        digest::<12>(),
        "orna-1",
        ExecutableArtifact::new(
            ExecutableArtifactKind::Client,
            "orna.client-plan",
            1,
            vec![1, 2, 3],
            digest::<10>(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn unaffected_function_revision() -> FunctionRevisionRecord {
    function_revision_fixture(
        FunctionId::from_bytes(id::<19>()),
        FunctionRevisionId::from_bytes(id::<22>()),
        digest::<21>(),
        digest::<22>(),
    )
}

fn mixed_function_catalogue(
    affected_revision: FunctionRevisionId,
    unaffected_revision: FunctionRevisionId,
) -> CatalogueSnapshot {
    let schema = SchemaDefinition::new(
        SchemaId::from_bytes(id::<8>()),
        QualifiedSemanticName::new(["crm"]).unwrap(),
    );
    let functions = [
        (
            FunctionId::from_bytes(id::<9>()),
            "lookup",
            affected_revision,
        ),
        (
            FunctionId::from_bytes(id::<19>()),
            "unchanged",
            unaffected_revision,
        ),
    ]
    .map(|(function, name, revision)| {
        FunctionDefinition::new(
            function,
            QualifiedSemanticName::new(["crm", name]).unwrap(),
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                "found",
                0,
                ResolvedType::value(TypeId::from_bytes(id::<71>())),
            )]),
            revision,
            FunctionSecurity::Invoker,
            None,
            FunctionVolatility::Stable,
        )
    });
    CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes(id::<7>()),
        vec![schema],
        vec![],
        functions.into(),
    )
    .unwrap()
}

fn mixed_function_origins(
    affected: &FunctionRevisionRecord,
    unaffected: &FunctionRevisionRecord,
) -> Vec<DefinitionOrigin> {
    let mut origins = function_origins(affected);
    origins.extend([
        DefinitionOrigin::new(
            DefinitionIdentity::Function(unaffected.function()),
            unaffected.declaration_origin(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::FunctionReturnColumn {
                owner: unaffected.function(),
                ordinal: 0,
            },
            unaffected.declaration_origin(),
        ),
    ]);
    origins
}

fn standard_context() -> CatalogueHashContext {
    standard_context_with_value_types(vec![standard_boolean_definition()])
}

fn standard_boolean_definition() -> ValueTypeDefinition {
    ValueTypeDefinition::primitive(
        TypeId::from_bytes(id::<71>()),
        QualifiedSemanticName::new(["std", "boolean"]).unwrap(),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.boolean@1",
    )
}

fn opaque_standard_context() -> CatalogueHashContext {
    standard_context_with_value_types(vec![
        standard_boolean_definition(),
        ValueTypeDefinition::opaque(
            TypeId::from_bytes(id::<72>()),
            QualifiedSemanticName::new(["std", "token"]).unwrap(),
            "std.token@1",
        ),
    ])
}

fn standard_context_with_value_types(
    value_types: Vec<ValueTypeDefinition>,
) -> CatalogueHashContext {
    let unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes(id::<3>()),
        0,
        "std/types.orna",
        "CREATE SCHEMA std; CREATE TYPE std.boolean;",
        source_unit_content_digest("CREATE SCHEMA std; CREATE TYPE std.boolean;").unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes(id::<1>()),
        SourceRevisionId::from_bytes(id::<2>()),
        None,
        vec![unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes(id::<1>()), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let value_type_ids = value_types
        .iter()
        .map(ValueTypeDefinition::id)
        .collect::<Vec<_>>();
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes(id::<72>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<73>()),
            QualifiedSemanticName::new(["std"]).unwrap(),
        )],
        vec![],
        value_types,
        vec![],
    )
    .unwrap();
    let mut origins = vec![DefinitionOrigin::new(
        DefinitionIdentity::Schema(SchemaId::from_bytes(id::<73>())),
        SourceOrigin::new(SourceUnitId::from_bytes(id::<3>()), 0, 1).unwrap(),
    )];
    origins.extend(
        value_type_ids
            .into_iter()
            .enumerate()
            .map(|(index, value_type_id)| {
                DefinitionOrigin::new(
                    DefinitionIdentity::ValueType(value_type_id),
                    SourceOrigin::new(
                        SourceUnitId::from_bytes(id::<3>()),
                        index as u32 + 1,
                        index as u32 + 2,
                    )
                    .unwrap(),
                )
            }),
    );
    let provisional = StandardLibrarySnapshot::new(
        StandardLibraryRevisionId::from_bytes(id::<74>()),
        StandardLibraryDigestVersion::Version1,
        source.clone(),
        "orna.language/1",
        catalogue.clone(),
        origins.clone(),
        digest::<75>(),
    )
    .unwrap();
    let digest = calculate_standard_library_digest_for_test(&provisional).unwrap();
    let standard = StandardLibrarySnapshot::new(
        provisional.revision(),
        provisional.digest_version(),
        source,
        provisional.language_version(),
        catalogue,
        origins,
        digest,
    )
    .unwrap();
    CatalogueHashContext::version_two(verify_standard_library_snapshot(standard).unwrap())
}

fn value_type_catalogue() -> CatalogueSnapshot {
    CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes(id::<7>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<8>()),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![],
        vec![ValueTypeDefinition::primitive(
            TypeId::from_bytes(id::<71>()),
            QualifiedSemanticName::new(["crm", "flag"]).unwrap(),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.flag@1",
        )],
        vec![],
    )
    .unwrap()
}

fn enum_type_catalogue() -> CatalogueSnapshot {
    CatalogueSnapshot::new_with_enum_types(
        CatalogueRevisionId::from_bytes(id::<7>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<8>()),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![EnumTypeDefinition::new(
            TypeId::from_bytes(id::<71>()),
            QualifiedSemanticName::new(["crm", "stage"]).unwrap(),
            ["lead", "customer"],
        )],
        vec![],
    )
    .unwrap()
}

fn record_value_type_catalogue() -> CatalogueSnapshot {
    CatalogueSnapshot::new_with_record_value_types(
        CatalogueRevisionId::from_bytes(id::<7>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<8>()),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![],
        vec![RecordValueTypeDefinition::new(
            TypeId::from_bytes(id::<76>()),
            QualifiedSemanticName::new(["crm", "status"]).unwrap(),
            vec![
                RecordValueFieldDefinition::try_new_descriptor(
                    FieldId::from_bytes(id::<77>()),
                    "active",
                    0,
                    TypeDescriptor::named(TypeId::from_bytes(id::<71>())),
                )
                .unwrap(),
            ],
        )],
        vec![],
    )
    .unwrap()
}

#[test]
fn revision_references_accept_exact_record_value_fields() {
    let catalogue = record_value_type_catalogue();
    let target = DefinitionReferenceTarget::Field {
        owner: TypeId::from_bytes(id::<76>()),
        field: FieldId::from_bytes(id::<77>()),
    };
    assert!(reference_target_exists(
        &catalogue,
        None,
        None,
        &HashSet::new(),
        target,
    ));
    assert!(!reference_target_exists(
        &catalogue,
        None,
        None,
        &HashSet::new(),
        DefinitionReferenceTarget::Field {
            owner: TypeId::from_bytes(id::<76>()),
            field: FieldId::from_bytes(id::<78>()),
        },
    ));
}

#[test]
fn enum_types_use_value_origins_and_require_the_version_two_catalogue_contract() {
    let catalogue = enum_type_catalogue();
    let enum_identity = DefinitionIdentity::ValueType(TypeId::from_bytes(id::<71>()));
    assert!(expected_definition_identities(&catalogue, &[]).contains(&enum_identity));
    assert!(definition_exists(
        &catalogue,
        &HashSet::new(),
        enum_identity
    ));
    assert!(matches!(
        validate_catalogue_hash_context_version_one(&catalogue, &[], &[], &[]),
        Err(RevisionInvariantError::ValueTypeDefinitionRequiresCatalogueHashVersionTwo {
            value_type,
        }) if value_type == TypeId::from_bytes(id::<71>())
    ));
}

fn value_type_origins() -> Vec<DefinitionOrigin> {
    let source = SourceUnitId::from_bytes(id::<3>());
    vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(SchemaId::from_bytes(id::<8>())),
            SourceOrigin::new(source, 0, 1).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(TypeId::from_bytes(id::<71>())),
            SourceOrigin::new(source, 1, 2).unwrap(),
        ),
    ]
}

fn binding_catalogue() -> (CatalogueSnapshot, TypeBindingId) {
    let binding = TypeBinding::qualified(
        QualifiedSemanticName::new(["crm", "contact_alias"]).unwrap(),
        TypeId::from_bytes(id::<12>()),
    )
    .unwrap();
    let binding_id = binding.id();
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes(id::<7>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<8>()),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![ObjectTypeDefinition::new(
            TypeId::from_bytes(id::<12>()),
            QualifiedSemanticName::new(["crm", "contact"]).unwrap(),
            vec![],
        )],
        vec![],
        vec![binding],
    )
    .unwrap();
    (catalogue, binding_id)
}

fn binding_origins(binding: TypeBindingId) -> Vec<DefinitionOrigin> {
    let source = SourceUnitId::from_bytes(id::<3>());
    vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(SchemaId::from_bytes(id::<8>())),
            SourceOrigin::new(source, 0, 1).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ObjectType(TypeId::from_bytes(id::<12>())),
            SourceOrigin::new(source, 1, 2).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::TypeBinding(binding),
            SourceOrigin::new(source, 2, 3).unwrap(),
        ),
    ]
}

fn historical_function_revision(
    function: FunctionId,
    revision: FunctionRevisionId,
    revision_number: u64,
    source_unit: SourceUnitId,
) -> FunctionRevisionRecord {
    FunctionRevisionRecord::new(
        function,
        revision,
        revision_number,
        SourceOrigin::new(source_unit, 4, 23).unwrap(),
        digest::<31>(),
        digest::<32>(),
        "orna-1",
        artifact(),
    )
    .unwrap()
}

fn active_with_history(
    historical_function_revisions: Vec<FunctionRevisionRecord>,
) -> Result<ActiveDatabaseRevision, RevisionInvariantError> {
    let source = source(None);
    let current_revision = function_revision();
    let catalogue = function_catalogue(current_revision.id());
    let origins = function_origins(&current_revision);
    let pair = RevisionPair::new(source.id(), catalogue.revision());

    ActiveDatabaseRevision::new_with_history(
        pair,
        source,
        catalogue,
        digest::<7>(),
        vec![],
        vec![current_revision],
        historical_function_revisions,
        origins,
        vec![],
    )
}

fn function_origins(revision: &FunctionRevisionRecord) -> Vec<DefinitionOrigin> {
    vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(SchemaId::from_bytes(id::<8>())),
            SourceOrigin::new(SourceUnitId::from_bytes(id::<3>()), 0, 18).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Function(revision.function()),
            revision.declaration_origin(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::FunctionReturnColumn {
                owner: revision.function(),
                ordinal: 0,
            },
            revision.declaration_origin(),
        ),
    ]
}

fn write_origins(revision: &FunctionRevisionRecord) -> Vec<DefinitionOrigin> {
    let mut origins = function_origins(revision);
    let source = SourceUnitId::from_bytes(id::<3>());
    origins.extend([
        DefinitionOrigin::new(
            DefinitionIdentity::ObjectType(TypeId::from_bytes(id::<12>())),
            SourceOrigin::new(source, 0, 10).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Field {
                owner: TypeId::from_bytes(id::<12>()),
                field: FieldId::from_bytes(id::<13>()),
            },
            SourceOrigin::new(source, 10, 18).unwrap(),
        ),
    ]);
    origins
}

#[test]
fn retains_an_empty_active_revision_without_inventing_source() {
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes(id::<1>()),
        SourceRevisionId::from_bytes(id::<2>()),
        None,
        vec![],
        digest::<1>(),
        digest::<2>(),
    )
    .unwrap();
    let catalogue = empty_catalogue();
    let pair = RevisionPair::new(source.id(), catalogue.revision());

    let active = ActiveDatabaseRevision::new(
        pair,
        source,
        catalogue,
        digest::<7>(),
        vec![],
        vec![],
        vec![],
        vec![],
    )
    .unwrap();

    assert!(active.source().units().is_empty());
    assert!(active.function_revisions().is_empty());
    assert!(active.historical_function_revisions().is_empty());
    assert_eq!(active.pair(), pair);
    assert_eq!(active.catalogue_hash(), digest::<7>());
    assert_eq!(
        active.catalogue_hash_context().version(),
        CatalogueHashVersion::Version1
    );
}

#[test]
fn active_and_deployable_revisions_reject_the_reserved_health_function_identity() {
    let function = crate::security::CATALOGUE_HEALTH_FUNCTION_ID;
    let revision = function_revision_fixture(
        function,
        FunctionRevisionId::from_bytes(id::<90>()),
        digest::<90>(),
        digest::<91>(),
    );
    let catalogue = function_catalogue_with_identity(
        function,
        revision.id(),
        vec![],
        ResolvedType::scalar(StandardScalar::Boolean),
    );
    let origins = function_origins(&revision);
    let expected = RevisionInvariantError::ReservedSystemFunctionIdentity { function };
    let active_source = source(None);

    let active_error = ActiveDatabaseRevision::new(
        RevisionPair::new(active_source.id(), catalogue.revision()),
        active_source,
        catalogue.clone(),
        digest::<7>(),
        vec![],
        vec![revision.clone()],
        origins.clone(),
        vec![],
    )
    .unwrap_err();
    assert_eq!(active_error, expected);

    let expected_base = RevisionPair::new(
        SourceRevisionId::from_bytes(id::<20>()),
        CatalogueRevisionId::from_bytes(id::<21>()),
    );
    let deployable_error = DeployableRevision::new(
        expected_base,
        source(Some(expected_base.source())),
        expected_base.catalogue(),
        catalogue,
        digest::<7>(),
        origins,
        vec![],
        vec![revision],
        vec![],
    )
    .unwrap_err();
    assert_eq!(deployable_error, expected);
    assert_eq!(
        deployable_error.to_string(),
        "the reserved system function identity cannot enter an application catalogue"
    );
}

#[test]
fn legacy_function_revision_constructor_defaults_to_semantic_hash_version_one() {
    assert_eq!(
        function_revision().semantic_hash_version(),
        FunctionSemanticHashVersion::Version1
    );
}

#[test]
fn active_version_one_rejects_a_version_two_function_revision() {
    let source = source(None);
    let revision = function_revision_v2();
    let catalogue = function_catalogue(revision.id());
    let origins = function_origins(&revision);
    let pair = RevisionPair::new(source.id(), catalogue.revision());

    let result = ActiveDatabaseRevision::new(
        pair,
        source,
        catalogue,
        digest::<7>(),
        vec![],
        vec![revision.clone()],
        origins,
        vec![],
    );

    assert!(matches!(
        result,
        Err(
            RevisionInvariantError::FunctionSemanticHashVersionRequiresCatalogueHashVersionTwo {
                function,
                revision: rejected_revision,
            }
        ) if function == revision.function() && rejected_revision == revision.id()
    ));
}

#[test]
fn active_version_one_rejects_each_version_two_catalogue_fact() {
    let active_source = source(None);
    let catalogue = value_type_catalogue();
    let pair = RevisionPair::new(active_source.id(), catalogue.revision());
    assert!(matches!(
        ActiveDatabaseRevision::new(
            pair,
            active_source,
            catalogue,
            digest::<7>(),
            vec![],
            vec![],
            value_type_origins(),
            vec![],
        ),
        Err(RevisionInvariantError::ValueTypeDefinitionRequiresCatalogueHashVersionTwo {
            value_type,
        }) if value_type == TypeId::from_bytes(id::<71>())
    ));

    let active_source = source(None);
    let (catalogue, binding) = binding_catalogue();
    let pair = RevisionPair::new(active_source.id(), catalogue.revision());
    assert!(matches!(
        ActiveDatabaseRevision::new(
            pair,
            active_source,
            catalogue,
            digest::<7>(),
            vec![],
            vec![],
            binding_origins(binding),
            vec![],
        ),
        Err(RevisionInvariantError::TypeBindingRequiresCatalogueHashVersionTwo {
            binding: rejected,
        }) if rejected == binding
    ));

    for identity in [
        DefinitionIdentity::ValueType(TypeId::from_bytes(id::<71>())),
        DefinitionIdentity::TypeBinding(TypeBindingId::from_bytes(id::<72>())),
    ] {
        let active_source = source(None);
        let catalogue = empty_catalogue();
        let pair = RevisionPair::new(active_source.id(), catalogue.revision());
        let origin = DefinitionOrigin::new(
            identity,
            SourceOrigin::new(SourceUnitId::from_bytes(id::<3>()), 0, 1).unwrap(),
        );
        assert!(matches!(
            ActiveDatabaseRevision::new(
                pair,
                active_source,
                catalogue,
                digest::<7>(),
                vec![],
                vec![],
                vec![origin],
                vec![],
            ),
            Err(RevisionInvariantError::DefinitionOriginRequiresCatalogueHashVersionTwo {
                identity: rejected,
            }) if rejected == identity
        ));
    }

    let active_source = source(None);
    let revision = function_revision();
    let catalogue = function_catalogue(revision.id());
    let pair = RevisionPair::new(active_source.id(), catalogue.revision());
    let target = TypeId::from_bytes(id::<71>());
    let reference = DefinitionReference::new(
        revision.function(),
        revision.id(),
        0,
        DefinitionReferenceTarget::ValueType(target),
        DefinitionReferenceKind::NamedType,
        revision.declaration_origin(),
    );
    assert!(matches!(
        ActiveDatabaseRevision::new(
            pair,
            active_source,
            catalogue,
            digest::<7>(),
            vec![],
            vec![revision.clone()],
            function_origins(&revision),
            vec![reference],
        ),
        Err(RevisionInvariantError::ValueTypeReferenceRequiresCatalogueHashVersionTwo {
            function,
            revision: rejected_revision,
            target: rejected_target,
        }) if function == revision.function()
            && rejected_revision == revision.id()
            && rejected_target == target
    ));
}

#[test]
fn active_version_two_requires_value_type_reference_owners_to_use_semantic_version_two() {
    let active_source = source(None);
    let revision = function_revision();
    let catalogue = function_catalogue_v2(revision.id());
    let pair = RevisionPair::new(active_source.id(), catalogue.revision());
    let target = TypeId::from_bytes(id::<71>());
    let reference = DefinitionReference::new(
        revision.function(),
        revision.id(),
        0,
        DefinitionReferenceTarget::ValueType(target),
        DefinitionReferenceKind::NamedType,
        revision.declaration_origin(),
    );
    let input = ActiveDatabaseRevisionInput::new(
        pair,
        active_source,
        catalogue,
        digest::<7>(),
        ActiveRevisionContent::new(
            vec![],
            vec![revision.clone()],
            function_origins(&revision),
            vec![reference],
        ),
    );

    assert!(matches!(
        ActiveDatabaseRevision::new_with_catalogue_hash_context(input, standard_context()),
        Err(RevisionInvariantError::ValueTypeReferenceRequiresFunctionSemanticHashVersionTwo {
            function,
            revision: rejected_revision,
            target: rejected_target,
        }) if function == revision.function()
            && rejected_revision == revision.id()
            && rejected_target == target
    ));
}

#[test]
fn version_two_active_revision_accepts_a_standard_value_type_target() {
    let source = source(None);
    let revision = function_revision_v2();
    let catalogue = function_catalogue_v2(revision.id());
    let pair = RevisionPair::new(source.id(), catalogue.revision());
    let reference = DefinitionReference::new(
        revision.function(),
        revision.id(),
        0,
        DefinitionReferenceTarget::ValueType(TypeId::from_bytes(id::<71>())),
        DefinitionReferenceKind::NamedType,
        revision.declaration_origin(),
    );

    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            pair,
            source,
            catalogue,
            digest::<7>(),
            ActiveRevisionContent::new(
                vec![],
                vec![revision],
                function_origins(&function_revision_v2()),
                vec![reference],
            ),
        ),
        standard_context(),
    )
    .unwrap();

    assert_eq!(
        active.catalogue_hash_context().version(),
        CatalogueHashVersion::Version2
    );
    assert!(active.catalogue_hash_context().standard().is_some());
}

#[test]
fn retains_history_for_removed_functions_with_earlier_source_origins() {
    let removed_function = FunctionId::from_bytes(id::<40>());
    let earlier_source_unit = SourceUnitId::from_bytes(id::<41>());
    let historical_revision = historical_function_revision(
        removed_function,
        FunctionRevisionId::from_bytes(id::<42>()),
        7,
        earlier_source_unit,
    );

    let active = active_with_history(vec![historical_revision.clone()]).unwrap();

    assert_eq!(
        active.historical_function_revisions(),
        [historical_revision]
    );
    assert_eq!(
        active.historical_function_revisions()[0]
            .declaration_origin()
            .source_unit(),
        earlier_source_unit
    );
    assert!(
        active
            .catalogue()
            .function_by_id(removed_function)
            .is_none()
    );
}

#[test]
fn rejects_function_revision_id_reused_between_current_and_history() {
    let current_revision = function_revision();
    let duplicate = historical_function_revision(
        FunctionId::from_bytes(id::<43>()),
        current_revision.id(),
        2,
        SourceUnitId::from_bytes(id::<44>()),
    );

    let result = active_with_history(vec![duplicate]);

    assert!(matches!(
        result,
        Err(RevisionInvariantError::DuplicateFunctionRevisionId { revision })
            if revision == current_revision.id()
    ));
}

#[test]
fn rejects_function_revision_number_reused_between_current_and_history() {
    let current_revision = function_revision();
    let duplicate = historical_function_revision(
        current_revision.function(),
        FunctionRevisionId::from_bytes(id::<45>()),
        current_revision.revision_number(),
        SourceUnitId::from_bytes(id::<46>()),
    );

    let result = active_with_history(vec![duplicate]);

    assert!(matches!(
        result,
        Err(RevisionInvariantError::DuplicateFunctionRevisionNumber {
            function,
            revision_number,
        }) if function == current_revision.function()
            && revision_number == current_revision.revision_number()
    ));
}

#[test]
fn rejects_function_revision_hash_pair_reused_between_current_and_history() {
    let current_revision = function_revision();
    let duplicate = FunctionRevisionRecord::new(
        current_revision.function(),
        FunctionRevisionId::from_bytes(id::<47>()),
        2,
        SourceOrigin::new(SourceUnitId::from_bytes(id::<48>()), 4, 23).unwrap(),
        current_revision.declaration_content_hash(),
        current_revision.semantic_hash(),
        current_revision.language_version(),
        current_revision.artifact().clone(),
    )
    .unwrap();

    let result = active_with_history(vec![duplicate]);

    assert!(matches!(
        result,
        Err(RevisionInvariantError::DuplicateFunctionRevisionHashPair {
            function,
            declaration_content_hash,
            semantic_hash,
        }) if function == current_revision.function()
            && declaration_content_hash == current_revision.declaration_content_hash()
            && semantic_hash == current_revision.semantic_hash()
    ));
}

#[test]
fn rejects_function_revision_hash_pair_reused_within_history() {
    let function = FunctionId::from_bytes(id::<49>());
    let first = historical_function_revision(
        function,
        FunctionRevisionId::from_bytes(id::<50>()),
        1,
        SourceUnitId::from_bytes(id::<51>()),
    );
    let duplicate = historical_function_revision(
        function,
        FunctionRevisionId::from_bytes(id::<52>()),
        2,
        SourceUnitId::from_bytes(id::<53>()),
    );

    let result = active_with_history(vec![first.clone(), duplicate]);

    assert!(matches!(
        result,
        Err(RevisionInvariantError::DuplicateFunctionRevisionHashPair {
            function: rejected_function,
            declaration_content_hash,
            semantic_hash,
        }) if rejected_function == function
            && declaration_content_hash == first.declaration_content_hash()
            && semantic_hash == first.semantic_hash()
    ));
}

#[test]
fn retains_exact_utf8_source_and_validates_its_byte_origins() {
    let source = source(None);
    let catalogue = function_catalogue(FunctionRevisionId::from_bytes(id::<11>()));
    let revision = function_revision();
    let origins = function_origins(&revision);
    let pair = RevisionPair::new(source.id(), catalogue.revision());

    let active = ActiveDatabaseRevision::new(
        pair,
        source,
        catalogue,
        digest::<7>(),
        vec![],
        vec![revision],
        origins,
        vec![],
    )
    .unwrap();

    assert_eq!(
        active.source().units()[1].content(),
        "-- cafe\u{301}\nFUNCTION crm.lookup;\n"
    );
    assert_eq!(
        active.function_revisions()[0].artifact().format(),
        "orna.server-plan"
    );
}

#[test]
fn retains_the_historical_origin_of_a_reused_function_revision() {
    let source = source(None);
    let catalogue = function_catalogue(FunctionRevisionId::from_bytes(id::<11>()));
    let current_revision = function_revision();
    let historical_origin = SourceOrigin::new(SourceUnitId::from_bytes(id::<99>()), 4, 23)
        .expect("historical origin is ordered");
    let reused_revision = FunctionRevisionRecord::new(
        current_revision.function(),
        current_revision.id(),
        current_revision.revision_number(),
        historical_origin,
        current_revision.declaration_content_hash(),
        current_revision.semantic_hash(),
        current_revision.language_version(),
        current_revision.artifact().clone(),
    )
    .unwrap();
    let origins = function_origins(&current_revision);
    let pair = RevisionPair::new(source.id(), catalogue.revision());

    let active = ActiveDatabaseRevision::new(
        pair,
        source,
        catalogue,
        digest::<7>(),
        vec![],
        vec![reused_revision],
        origins,
        vec![],
    )
    .unwrap();

    assert_eq!(
        active.function_revisions()[0].declaration_origin(),
        historical_origin
    );
}

#[test]
fn rejects_structural_revision_inconsistencies() {
    let duplicate = StoredSourceRevision::new(
        SourceBundleId::from_bytes(id::<1>()),
        SourceRevisionId::from_bytes(id::<2>()),
        None,
        vec![
            StoredSourceUnit::new(
                SourceUnitId::from_bytes(id::<3>()),
                0,
                "a",
                "",
                digest::<1>(),
            )
            .unwrap(),
            StoredSourceUnit::new(
                SourceUnitId::from_bytes(id::<3>()),
                1,
                "b",
                "",
                digest::<2>(),
            )
            .unwrap(),
        ],
        digest::<3>(),
        digest::<4>(),
    );
    assert_eq!(
        duplicate,
        Err(RevisionInvariantError::DuplicateSourceUnitId {
            source_unit: SourceUnitId::from_bytes(id::<3>())
        })
    );

    let source_without_origins = source(None);
    let catalogue_without_origins = function_catalogue(FunctionRevisionId::from_bytes(id::<11>()));
    let pair_without_origins = RevisionPair::new(
        source_without_origins.id(),
        catalogue_without_origins.revision(),
    );
    assert!(matches!(
        ActiveDatabaseRevision::new(
            pair_without_origins,
            source_without_origins,
            catalogue_without_origins,
            digest::<7>(),
            vec![],
            vec![function_revision()],
            vec![],
            vec![],
        ),
        Err(RevisionInvariantError::MissingDefinitionOrigin { .. })
    ));

    let invalid_source = source(None);
    let catalogue = function_catalogue(FunctionRevisionId::from_bytes(id::<11>()));
    let bad_origin = FunctionRevisionRecord::new(
        FunctionId::from_bytes(id::<9>()),
        FunctionRevisionId::from_bytes(id::<11>()),
        1,
        SourceOrigin::new(SourceUnitId::from_bytes(id::<4>()), 7, 8).unwrap(),
        digest::<1>(),
        digest::<2>(),
        "orna-1",
        artifact(),
    )
    .unwrap();
    let pair = RevisionPair::new(invalid_source.id(), catalogue.revision());
    assert!(matches!(
        ActiveDatabaseRevision::new(
            pair,
            invalid_source,
            catalogue,
            digest::<7>(),
            vec![],
            vec![bad_origin.clone()],
            function_origins(&bad_origin),
            vec![]
        ),
        Err(RevisionInvariantError::SourceOriginNotCharacterBoundary { .. })
    ));
}

#[test]
fn rejects_stale_parent_and_duplicate_reference_ordinals() {
    let expected = RevisionPair::new(
        SourceRevisionId::from_bytes(id::<20>()),
        CatalogueRevisionId::from_bytes(id::<21>()),
    );
    let rejected_source = source(None);
    let catalogue = function_catalogue(FunctionRevisionId::from_bytes(id::<11>()));
    assert!(matches!(
        DeployableRevision::new(
            expected,
            rejected_source,
            expected.catalogue(),
            catalogue,
            digest::<7>(),
            vec![],
            vec![],
            vec![],
            vec![]
        ),
        Err(RevisionInvariantError::DeployableSourceParentMismatch { .. })
    ));

    let current_revision = function_revision();
    let moved_new_revision = FunctionRevisionRecord::new(
        current_revision.function(),
        current_revision.id(),
        current_revision.revision_number(),
        SourceOrigin::new(SourceUnitId::from_bytes(id::<4>()), 11, 29).unwrap(),
        current_revision.declaration_content_hash(),
        current_revision.semantic_hash(),
        current_revision.language_version(),
        current_revision.artifact().clone(),
    )
    .unwrap();
    assert!(matches!(
        DeployableRevision::new(
            expected,
            source(Some(expected.source())),
            expected.catalogue(),
            function_catalogue(current_revision.id()),
            digest::<7>(),
            function_origins(&current_revision),
            vec![],
            vec![moved_new_revision],
            vec![],
        ),
        Err(RevisionInvariantError::FunctionRevisionOriginMismatch { .. })
    ));

    let duplicate_reference_source = source(Some(expected.source()));
    let catalogue = function_catalogue(FunctionRevisionId::from_bytes(id::<11>()));
    let revision = function_revision();
    let reference = DefinitionReference::new(
        revision.function(),
        revision.id(),
        0,
        DefinitionReferenceTarget::Function(revision.function()),
        DefinitionReferenceKind::FunctionCall,
        revision.declaration_origin(),
    );
    assert!(matches!(
        DeployableRevision::new(
            expected,
            duplicate_reference_source,
            expected.catalogue(),
            catalogue,
            digest::<7>(),
            function_origins(&revision),
            vec![],
            vec![revision],
            vec![reference.clone(), reference],
        ),
        Err(RevisionInvariantError::DuplicateReferenceOrdinal { .. })
    ));

    let unknown_target_revision = function_revision();
    let unknown_target = DefinitionReference::new(
        unknown_target_revision.function(),
        unknown_target_revision.id(),
        0,
        DefinitionReferenceTarget::Expression(ExpressionId::from_bytes(id::<99>())),
        DefinitionReferenceKind::Expression,
        unknown_target_revision.declaration_origin(),
    );
    assert!(matches!(
        DeployableRevision::new(
            expected,
            source(Some(expected.source())),
            expected.catalogue(),
            function_catalogue(FunctionRevisionId::from_bytes(id::<11>())),
            digest::<7>(),
            function_origins(&unknown_target_revision),
            vec![],
            vec![unknown_target_revision],
            vec![unknown_target],
        ),
        Err(RevisionInvariantError::ReferenceTargetNotInRevision { .. })
    ));

    let mismatched_revision = function_revision();
    let mismatched_reference = DefinitionReference::new(
        mismatched_revision.function(),
        mismatched_revision.id(),
        0,
        DefinitionReferenceTarget::Function(mismatched_revision.function()),
        DefinitionReferenceKind::QueryObject,
        mismatched_revision.declaration_origin(),
    );
    assert!(matches!(
        DeployableRevision::new(
            expected,
            source(Some(expected.source())),
            expected.catalogue(),
            function_catalogue(mismatched_revision.id()),
            digest::<7>(),
            function_origins(&mismatched_revision),
            vec![],
            vec![mismatched_revision],
            vec![mismatched_reference],
        ),
        Err(RevisionInvariantError::ReferenceKindTargetMismatch { .. })
    ));

    let candidate_revision = function_revision();
    let candidate_reference = DefinitionReference::new(
        candidate_revision.function(),
        candidate_revision.id(),
        0,
        DefinitionReferenceTarget::Function(candidate_revision.function()),
        DefinitionReferenceKind::FunctionCall,
        candidate_revision.declaration_origin(),
    );
    let deployable = DeployableRevision::new(
        expected,
        source(Some(expected.source())),
        expected.catalogue(),
        function_catalogue(FunctionRevisionId::from_bytes(id::<11>())),
        digest::<7>(),
        function_origins(&candidate_revision),
        vec![],
        vec![candidate_revision],
        vec![candidate_reference],
    )
    .unwrap();
    assert_eq!(
        deployable.candidate_pair(),
        RevisionPair::new(
            SourceRevisionId::from_bytes(id::<2>()),
            CatalogueRevisionId::from_bytes(id::<7>()),
        )
    );
    assert_eq!(deployable.catalogue_hash(), digest::<7>());
    assert_eq!(
        deployable.catalogue_hash_context().version(),
        CatalogueHashVersion::Version1
    );

    let revision = function_revision_v2();
    let standard_reference = DefinitionReference::new(
        revision.function(),
        revision.id(),
        0,
        DefinitionReferenceTarget::ValueType(TypeId::from_bytes(id::<71>())),
        DefinitionReferenceKind::NamedType,
        revision.declaration_origin(),
    );
    let deployable = DeployableRevision::new_with_catalogue_hash_context(
        DeployableRevisionInput::new(
            expected,
            source(Some(expected.source())),
            expected.catalogue(),
            function_catalogue_v2(revision.id()),
            digest::<7>(),
            DeployableRevisionContent::new(
                function_origins(&revision),
                vec![],
                vec![revision.clone()],
                vec![standard_reference],
            )
            .with_current_function_revisions(vec![revision]),
        ),
        standard_context(),
    )
    .unwrap();
    assert_eq!(
        deployable.catalogue_hash_context().version(),
        CatalogueHashVersion::Version2
    );
}

#[test]
fn deployable_version_one_rejects_each_version_two_catalogue_fact() {
    let expected = RevisionPair::new(
        SourceRevisionId::from_bytes(id::<20>()),
        CatalogueRevisionId::from_bytes(id::<21>()),
    );
    let catalogue = value_type_catalogue();
    assert!(matches!(
        DeployableRevision::new(
            expected,
            source(Some(expected.source())),
            expected.catalogue(),
            catalogue,
            digest::<7>(),
            value_type_origins(),
            vec![],
            vec![],
            vec![],
        ),
        Err(RevisionInvariantError::ValueTypeDefinitionRequiresCatalogueHashVersionTwo {
            value_type,
        }) if value_type == TypeId::from_bytes(id::<71>())
    ));

    let (catalogue, binding) = binding_catalogue();
    assert!(matches!(
        DeployableRevision::new(
            expected,
            source(Some(expected.source())),
            expected.catalogue(),
            catalogue,
            digest::<7>(),
            binding_origins(binding),
            vec![],
            vec![],
            vec![],
        ),
        Err(RevisionInvariantError::TypeBindingRequiresCatalogueHashVersionTwo {
            binding: rejected,
        }) if rejected == binding
    ));

    for identity in [
        DefinitionIdentity::ValueType(TypeId::from_bytes(id::<71>())),
        DefinitionIdentity::TypeBinding(TypeBindingId::from_bytes(id::<72>())),
    ] {
        let origin = DefinitionOrigin::new(
            identity,
            SourceOrigin::new(SourceUnitId::from_bytes(id::<3>()), 0, 1).unwrap(),
        );
        assert!(matches!(
            DeployableRevision::new(
                expected,
                source(Some(expected.source())),
                expected.catalogue(),
                empty_catalogue(),
                digest::<7>(),
                vec![origin],
                vec![],
                vec![],
                vec![],
            ),
            Err(RevisionInvariantError::DefinitionOriginRequiresCatalogueHashVersionTwo {
                identity: rejected,
            }) if rejected == identity
        ));
    }

    let revision = function_revision();
    let catalogue = function_catalogue(revision.id());
    let target = TypeId::from_bytes(id::<71>());
    let reference = DefinitionReference::new(
        revision.function(),
        revision.id(),
        0,
        DefinitionReferenceTarget::ValueType(target),
        DefinitionReferenceKind::NamedType,
        revision.declaration_origin(),
    );
    assert!(matches!(
        DeployableRevision::new(
            expected,
            source(Some(expected.source())),
            expected.catalogue(),
            catalogue,
            digest::<7>(),
            function_origins(&revision),
            vec![],
            vec![revision.clone()],
            vec![reference],
        ),
        Err(RevisionInvariantError::ValueTypeReferenceRequiresCatalogueHashVersionTwo {
            function,
            revision: rejected_revision,
            target: rejected_target,
        }) if function == revision.function()
            && rejected_revision == revision.id()
            && rejected_target == target
    ));

    let revision = function_revision_v2();
    let catalogue = function_catalogue(revision.id());
    assert!(matches!(
        DeployableRevision::new(
            expected,
            source(Some(expected.source())),
            expected.catalogue(),
            catalogue,
            digest::<7>(),
            function_origins(&revision),
            vec![],
            vec![revision.clone()],
            vec![],
        ),
        Err(
            RevisionInvariantError::FunctionSemanticHashVersionRequiresCatalogueHashVersionTwo {
                function,
                revision: rejected_revision,
            }
        ) if function == revision.function() && rejected_revision == revision.id()
    ));
}

#[test]
fn deployable_version_two_requires_value_type_reference_owners_to_use_semantic_version_two() {
    let expected = RevisionPair::new(
        SourceRevisionId::from_bytes(id::<20>()),
        CatalogueRevisionId::from_bytes(id::<21>()),
    );
    let revision = function_revision();
    let catalogue = function_catalogue_v2(revision.id());
    let target = TypeId::from_bytes(id::<71>());
    let reference = DefinitionReference::new(
        revision.function(),
        revision.id(),
        0,
        DefinitionReferenceTarget::ValueType(target),
        DefinitionReferenceKind::NamedType,
        revision.declaration_origin(),
    );
    let input = DeployableRevisionInput::new(
        expected,
        source(Some(expected.source())),
        expected.catalogue(),
        catalogue,
        digest::<7>(),
        DeployableRevisionContent::new(
            function_origins(&revision),
            vec![],
            vec![],
            vec![reference],
        )
        .with_current_function_revisions(vec![revision.clone()]),
    );

    assert!(matches!(
        DeployableRevision::new_with_catalogue_hash_context(input, standard_context()),
        Err(RevisionInvariantError::ValueTypeReferenceRequiresFunctionSemanticHashVersionTwo {
            function,
            revision: rejected_revision,
            target: rejected_target,
        }) if function == revision.function()
            && rejected_revision == revision.id()
            && rejected_target == target
    ));
}

#[test]
fn deployable_version_two_accepts_source_only_replay_with_a_reused_version_two_owner() {
    let expected = RevisionPair::new(
        SourceRevisionId::from_bytes(id::<20>()),
        CatalogueRevisionId::from_bytes(id::<21>()),
    );
    let revision = function_revision_v2();
    let target = TypeId::from_bytes(id::<71>());
    let reference = DefinitionReference::new(
        revision.function(),
        revision.id(),
        0,
        DefinitionReferenceTarget::ValueType(target),
        DefinitionReferenceKind::NamedType,
        revision.declaration_origin(),
    );
    let content = DeployableRevisionContent::new(
        function_origins(&revision),
        vec![],
        vec![],
        vec![reference],
    )
    .with_current_function_revisions(vec![revision.clone()]);
    let input = DeployableRevisionInput::new(
        expected,
        source(Some(expected.source())),
        expected.catalogue(),
        function_catalogue_v2(revision.id()),
        digest::<7>(),
        content,
    );

    let deployable =
        DeployableRevision::new_with_catalogue_hash_context(input, standard_context()).unwrap();

    assert!(deployable.new_function_revisions().is_empty());
    assert!(validate_persistable_catalogue(&deployable).is_ok());
    assert_eq!(
        deployable.current_function_revisions(),
        Some(&[revision][..])
    );
}

#[test]
fn deployable_version_two_accepts_unaffected_version_one_and_affected_version_two_current_revisions()
 {
    let expected = RevisionPair::new(
        SourceRevisionId::from_bytes(id::<20>()),
        CatalogueRevisionId::from_bytes(id::<21>()),
    );
    let affected = function_revision_v2();
    let unaffected = unaffected_function_revision();
    let reference = DefinitionReference::new(
        affected.function(),
        affected.id(),
        0,
        DefinitionReferenceTarget::ValueType(TypeId::from_bytes(id::<71>())),
        DefinitionReferenceKind::NamedType,
        affected.declaration_origin(),
    );
    let content = DeployableRevisionContent::new(
        mixed_function_origins(&affected, &unaffected),
        vec![],
        vec![],
        vec![reference],
    )
    .with_current_function_revisions(vec![unaffected.clone(), affected.clone()]);
    let input = DeployableRevisionInput::new(
        expected,
        source(Some(expected.source())),
        expected.catalogue(),
        mixed_function_catalogue(affected.id(), unaffected.id()),
        digest::<7>(),
        content,
    );

    let deployable =
        DeployableRevision::new_with_catalogue_hash_context(input, standard_context()).unwrap();

    assert!(deployable.new_function_revisions().is_empty());
    assert_eq!(
        deployable.current_function_revisions(),
        Some(&[unaffected, affected][..])
    );
}

#[test]
fn deployable_version_two_rejects_missing_and_crossed_current_revision_evidence() {
    let expected = RevisionPair::new(
        SourceRevisionId::from_bytes(id::<20>()),
        CatalogueRevisionId::from_bytes(id::<21>()),
    );
    let revision = function_revision_v2();
    let input_without_evidence = DeployableRevisionInput::new(
        expected,
        source(Some(expected.source())),
        expected.catalogue(),
        function_catalogue(revision.id()),
        digest::<7>(),
        DeployableRevisionContent::new(
            function_origins(&revision),
            vec![],
            vec![revision.clone()],
            vec![],
        ),
    );
    assert!(matches!(
        DeployableRevision::new_with_catalogue_hash_context(
            input_without_evidence,
            standard_context(),
        ),
        Err(RevisionInvariantError::DeployableCurrentFunctionRevisionsRequired)
    ));

    let input_with_missing_revision = DeployableRevisionInput::new(
        expected,
        source(Some(expected.source())),
        expected.catalogue(),
        function_catalogue(revision.id()),
        digest::<7>(),
        DeployableRevisionContent::new(function_origins(&revision), vec![], vec![], vec![])
            .with_current_function_revisions(vec![]),
    );
    assert!(matches!(
        DeployableRevision::new_with_catalogue_hash_context(
            input_with_missing_revision,
            standard_context(),
        ),
        Err(RevisionInvariantError::MissingDeployableCurrentFunctionRevision {
            function,
            revision: missing,
        }) if function == revision.function() && missing == revision.id()
    ));

    let crossed = FunctionRevisionRecord::new(
        revision.function(),
        revision.id(),
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        digest::<23>(),
        revision.language_version(),
        revision.artifact().clone(),
    )
    .unwrap()
    .with_semantic_hash_version(FunctionSemanticHashVersion::Version2);
    let input_with_crossed_revision = DeployableRevisionInput::new(
        expected,
        source(Some(expected.source())),
        expected.catalogue(),
        function_catalogue(revision.id()),
        digest::<7>(),
        DeployableRevisionContent::new(
            function_origins(&revision),
            vec![],
            vec![revision.clone()],
            vec![],
        )
        .with_current_function_revisions(vec![crossed]),
    );
    assert!(matches!(
        DeployableRevision::new_with_catalogue_hash_context(
            input_with_crossed_revision,
            standard_context(),
        ),
        Err(RevisionInvariantError::NewFunctionRevisionCurrentEvidenceMismatch {
            function,
            revision: crossed_revision,
        }) if function == revision.function() && crossed_revision == revision.id()
    ));
}

#[test]
fn revision_construction_validates_resolved_types_in_durable_slot_order() {
    let field_value = TypeId::from_bytes(id::<90>());
    let parameter_value = TypeId::from_bytes(id::<91>());
    let return_value = TypeId::from_bytes(id::<92>());
    let catalogue = resolved_type_slots_catalogue(
        ResolvedType::value(field_value),
        ResolvedType::value(parameter_value),
        FunctionReturn::Single(ResolvedType::value(return_value)),
    );
    let active_source = source(None);
    let input = ActiveDatabaseRevisionInput::new(
        RevisionPair::new(active_source.id(), catalogue.revision()),
        active_source,
        catalogue,
        digest::<7>(),
        ActiveRevisionContent::new(vec![], vec![], vec![], vec![]),
    );

    assert_eq!(
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            input.clone(),
            CatalogueHashContext::version_one(),
        )
        .unwrap_err(),
        RevisionInvariantError::ResolvedValueRequiresCatalogueHashVersionTwo {
            identity: DefinitionIdentity::Field {
                owner: TypeId::from_bytes(id::<80>()),
                field: FieldId::from_bytes(id::<81>()),
            },
            value_type: field_value,
        }
    );
    assert_eq!(
        ActiveDatabaseRevision::new_with_catalogue_hash_context(input, standard_context())
            .unwrap_err(),
        RevisionInvariantError::ResolvedValueTypeNotInPinnedStandard {
            identity: DefinitionIdentity::Field {
                owner: TypeId::from_bytes(id::<80>()),
                field: FieldId::from_bytes(id::<81>()),
            },
            value_type: field_value,
        }
    );

    let pinned_value = TypeId::from_bytes(id::<71>());
    let catalogue = resolved_type_slots_catalogue(
        ResolvedType::named(TypeId::from_bytes(id::<80>())),
        ResolvedType::value(parameter_value),
        FunctionReturn::Single(ResolvedType::value(pinned_value)),
    );
    assert_eq!(
        validate_resolved_type_slots(&CatalogueHashContext::version_one(), &catalogue,),
        Err(
            RevisionInvariantError::ResolvedValueRequiresCatalogueHashVersionTwo {
                identity: DefinitionIdentity::Parameter {
                    owner: FunctionId::from_bytes(id::<82>()),
                    parameter: ParameterId::from_bytes(id::<83>()),
                },
                value_type: parameter_value,
            },
        )
    );
    assert_eq!(
        validate_resolved_type_slots(&standard_context(), &catalogue,),
        Err(
            RevisionInvariantError::ResolvedValueTypeNotInPinnedStandard {
                identity: DefinitionIdentity::Parameter {
                    owner: FunctionId::from_bytes(id::<82>()),
                    parameter: ParameterId::from_bytes(id::<83>()),
                },
                value_type: parameter_value,
            }
        )
    );

    let single_value = TypeId::from_bytes(id::<93>());
    let catalogue = resolved_type_slots_catalogue(
        ResolvedType::named(TypeId::from_bytes(id::<80>())),
        ResolvedType::value(pinned_value),
        FunctionReturn::Single(ResolvedType::value(single_value)),
    );
    assert_eq!(
        validate_resolved_type_slots(&standard_context(), &catalogue,),
        Err(
            RevisionInvariantError::ResolvedValueTypeNotInPinnedStandard {
                identity: DefinitionIdentity::Function(FunctionId::from_bytes(id::<82>())),
                value_type: single_value,
            }
        )
    );

    let stream_value = TypeId::from_bytes(id::<95>());
    let catalogue = resolved_type_slots_catalogue(
        ResolvedType::named(TypeId::from_bytes(id::<80>())),
        ResolvedType::value(pinned_value),
        FunctionReturn::Stream(ResolvedType::value(stream_value)),
    );
    assert_eq!(
        validate_resolved_type_slots(&standard_context(), &catalogue,),
        Err(
            RevisionInvariantError::ResolvedValueTypeNotInPinnedStandard {
                identity: DefinitionIdentity::Function(FunctionId::from_bytes(id::<82>())),
                value_type: stream_value,
            }
        )
    );

    let rows_value = TypeId::from_bytes(id::<94>());
    let catalogue = resolved_type_slots_catalogue(
        ResolvedType::named(TypeId::from_bytes(id::<80>())),
        ResolvedType::value(pinned_value),
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "result",
            0,
            ResolvedType::value(rows_value),
        )]),
    );
    assert_eq!(
        validate_resolved_type_slots(&standard_context(), &catalogue,),
        Err(
            RevisionInvariantError::ResolvedValueTypeNotInPinnedStandard {
                identity: DefinitionIdentity::FunctionReturnColumn {
                    owner: FunctionId::from_bytes(id::<82>()),
                    ordinal: 0,
                },
                value_type: rows_value,
            }
        )
    );
}

#[test]
fn version_two_rejects_a_pinned_opaque_value_in_non_function_catalogue_slots() {
    let opaque = TypeId::from_bytes(id::<72>());
    let named = ResolvedType::named(TypeId::from_bytes(id::<80>()));
    let cases = [
        (
            resolved_type_slots_catalogue(
                ResolvedType::value(opaque),
                named,
                FunctionReturn::Single(named),
            ),
            DefinitionIdentity::Field {
                owner: TypeId::from_bytes(id::<80>()),
                field: FieldId::from_bytes(id::<81>()),
            },
        ),
        (
            resolved_type_slots_catalogue(
                named,
                ResolvedType::value(opaque),
                FunctionReturn::Single(named),
            ),
            DefinitionIdentity::Parameter {
                owner: FunctionId::from_bytes(id::<82>()),
                parameter: ParameterId::from_bytes(id::<83>()),
            },
        ),
        (
            resolved_type_slots_catalogue(
                named,
                named,
                FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                    "token",
                    0,
                    ResolvedType::value(opaque),
                )]),
            ),
            DefinitionIdentity::FunctionReturnColumn {
                owner: FunctionId::from_bytes(id::<82>()),
                ordinal: 0,
            },
        ),
    ];

    for (catalogue, identity) in cases {
        assert_eq!(
            validate_resolved_type_slots(&opaque_standard_context(), &catalogue),
            Err(RevisionInvariantError::OpaqueValueTypeNotAcceptedInSlot {
                identity,
                value_type: opaque,
            })
        );
    }
}

#[test]
fn version_two_accepts_only_the_exact_pinned_opaque_client_return() {
    let opaque = TypeId::from_bytes(id::<72>());
    let context = opaque_standard_context();
    let accepted = catalogue_with_opaque_client_return(
        opaque,
        FunctionDomain::Client,
        vec![],
        FunctionSecurity::Invoker,
        FunctionVolatility::Immutable,
    );
    assert_eq!(validate_resolved_type_slots(&context, &accepted), Ok(()));

    let parameter = ParameterDefinition::new(
        ParameterId::from_bytes(id::<83>()),
        "enabled",
        0,
        ResolvedType::value(TypeId::from_bytes(id::<71>())),
        None,
    );
    let parameterized = catalogue_with_opaque_client_return(
        opaque,
        FunctionDomain::Client,
        vec![parameter],
        FunctionSecurity::Invoker,
        FunctionVolatility::Immutable,
    );
    assert_eq!(
        validate_resolved_type_slots(&context, &parameterized),
        Ok(())
    );

    for catalogue in [
        catalogue_with_opaque_client_return(
            opaque,
            FunctionDomain::Server,
            vec![],
            FunctionSecurity::Invoker,
            FunctionVolatility::Immutable,
        ),
        catalogue_with_opaque_client_return(
            opaque,
            FunctionDomain::Client,
            vec![],
            FunctionSecurity::Definer,
            FunctionVolatility::Immutable,
        ),
        catalogue_with_opaque_client_return(
            opaque,
            FunctionDomain::Client,
            vec![],
            FunctionSecurity::Invoker,
            FunctionVolatility::Stable,
        ),
    ] {
        assert_eq!(
            validate_resolved_type_slots(&context, &catalogue),
            Err(RevisionInvariantError::OpaqueValueTypeNotAcceptedInSlot {
                identity: DefinitionIdentity::Function(FunctionId::from_bytes(id::<82>())),
                value_type: opaque,
            })
        );
    }
}

#[test]
fn active_and_deployable_revisions_accept_a_standalone_pinned_opaque_definition() {
    let active_source = source(None);
    let active_catalogue = empty_catalogue();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(active_source.id(), active_catalogue.revision()),
            active_source,
            active_catalogue,
            digest::<7>(),
            ActiveRevisionContent::new(vec![], vec![], vec![], vec![]),
        ),
        opaque_standard_context(),
    )
    .unwrap();
    let opaque = active
        .catalogue_hash_context()
        .standard()
        .unwrap()
        .catalogue()
        .value_type_by_id(TypeId::from_bytes(id::<72>()))
        .unwrap();
    assert_eq!(opaque.kind(), ValueTypeKind::Opaque);

    let expected = RevisionPair::new(
        SourceRevisionId::from_bytes(id::<20>()),
        CatalogueRevisionId::from_bytes(id::<21>()),
    );
    let deployable = DeployableRevision::new_with_catalogue_hash_context(
        DeployableRevisionInput::new(
            expected,
            source(Some(expected.source())),
            expected.catalogue(),
            empty_catalogue(),
            digest::<7>(),
            DeployableRevisionContent::new(vec![], vec![], vec![], vec![])
                .with_current_function_revisions(vec![]),
        ),
        opaque_standard_context(),
    )
    .unwrap();
    assert_eq!(
        deployable
            .catalogue_hash_context()
            .standard()
            .unwrap()
            .catalogue()
            .value_type_by_id(TypeId::from_bytes(id::<72>()))
            .unwrap()
            .kind(),
        ValueTypeKind::Opaque
    );
}

#[test]
fn version_one_rejects_an_opaque_definition_before_slot_validation() {
    let opaque = TypeId::from_bytes(id::<72>());
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes(id::<7>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<8>()),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![ObjectTypeDefinition::new(
            TypeId::from_bytes(id::<80>()),
            QualifiedSemanticName::new(["crm", "task"]).unwrap(),
            vec![FieldDefinition::new(
                FieldId::from_bytes(id::<81>()),
                "value",
                0,
                ResolvedType::value(TypeId::from_bytes(id::<99>())),
                false,
                false,
                None,
                None,
            )],
        )],
        vec![ValueTypeDefinition::opaque(
            opaque,
            QualifiedSemanticName::new(["crm", "token"]).unwrap(),
            "crm.token@1",
        )],
        vec![],
    )
    .unwrap();

    assert_eq!(
        validate_catalogue_hash_context_coherence(
            &CatalogueHashContext::version_one(),
            &catalogue,
            &[],
            &[],
            &[],
        ),
        Err(
            RevisionInvariantError::ValueTypeDefinitionRequiresCatalogueHashVersionTwo {
                value_type: opaque,
            }
        )
    );
}

#[test]
fn constructors_reject_version_two_scalars_in_each_durable_slot_order() {
    let pinned = ResolvedType::value(TypeId::from_bytes(id::<71>()));
    let scalar = ResolvedType::scalar(StandardScalar::Boolean);
    let cases = [
        (
            resolved_type_slots_catalogue(scalar, pinned, FunctionReturn::Single(pinned)),
            DefinitionIdentity::Field {
                owner: TypeId::from_bytes(id::<80>()),
                field: FieldId::from_bytes(id::<81>()),
            },
        ),
        (
            resolved_type_slots_catalogue(
                ResolvedType::named(TypeId::from_bytes(id::<80>())),
                scalar,
                FunctionReturn::Single(pinned),
            ),
            DefinitionIdentity::Parameter {
                owner: FunctionId::from_bytes(id::<82>()),
                parameter: ParameterId::from_bytes(id::<83>()),
            },
        ),
        (
            resolved_type_slots_catalogue(
                ResolvedType::named(TypeId::from_bytes(id::<80>())),
                pinned,
                FunctionReturn::Single(scalar),
            ),
            DefinitionIdentity::Function(FunctionId::from_bytes(id::<82>())),
        ),
        (
            resolved_type_slots_catalogue(
                ResolvedType::named(TypeId::from_bytes(id::<80>())),
                pinned,
                FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                    "result", 0, scalar,
                )]),
            ),
            DefinitionIdentity::FunctionReturnColumn {
                owner: FunctionId::from_bytes(id::<82>()),
                ordinal: 0,
            },
        ),
    ];
    for (catalogue, identity) in cases {
        let active_source = source(None);
        let active_input = ActiveDatabaseRevisionInput::new(
            RevisionPair::new(active_source.id(), catalogue.revision()),
            active_source,
            catalogue.clone(),
            digest::<7>(),
            ActiveRevisionContent::new(vec![], vec![], vec![], vec![]),
        );
        let active_error = ActiveDatabaseRevision::new_with_catalogue_hash_context(
            active_input,
            standard_context(),
        )
        .unwrap_err();
        let expected = RevisionInvariantError::LegacyScalarRequiresCatalogueHashVersionOne {
            identity,
            scalar: StandardScalar::Boolean,
        };
        assert_eq!(active_error, expected);
        assert_eq!(active_error.to_string(), expected.to_string());
        assert!(Error::source(&active_error).is_none());

        let expected_pair = RevisionPair::new(
            SourceRevisionId::from_bytes(id::<20>()),
            CatalogueRevisionId::from_bytes(id::<21>()),
        );
        let deployable_input = DeployableRevisionInput::new(
            expected_pair,
            source(Some(expected_pair.source())),
            expected_pair.catalogue(),
            catalogue,
            digest::<7>(),
            DeployableRevisionContent::new(vec![], vec![], vec![], vec![])
                .with_current_function_revisions(vec![resolved_slot_function_revision()]),
        );
        let deployable_error = DeployableRevision::new_with_catalogue_hash_context(
            deployable_input,
            standard_context(),
        )
        .unwrap_err();
        assert_eq!(deployable_error, expected);
        assert_eq!(deployable_error.to_string(), expected.to_string());
        assert!(Error::source(&deployable_error).is_none());
    }
}

#[test]
fn constructors_reject_version_two_scalars_including_client_parameters() {
    let expected = RevisionPair::new(
        SourceRevisionId::from_bytes(id::<20>()),
        CatalogueRevisionId::from_bytes(id::<21>()),
    );
    let revision = function_revision_v2();
    let input = DeployableRevisionInput::new(
        expected,
        source(Some(expected.source())),
        expected.catalogue(),
        function_catalogue(revision.id()),
        digest::<7>(),
        DeployableRevisionContent::new(function_origins(&revision), vec![], vec![], vec![])
            .with_current_function_revisions(vec![revision]),
    );
    assert_eq!(
        DeployableRevision::new_with_catalogue_hash_context(input, standard_context()).unwrap_err(),
        RevisionInvariantError::LegacyScalarRequiresCatalogueHashVersionOne {
            identity: DefinitionIdentity::FunctionReturnColumn {
                owner: FunctionId::from_bytes(id::<9>()),
                ordinal: 0,
            },
            scalar: StandardScalar::Boolean,
        }
    );

    let hostile_client = resolved_type_slots_catalogue(
        ResolvedType::named(TypeId::from_bytes(id::<80>())),
        ResolvedType::scalar(StandardScalar::Integer),
        FunctionReturn::Single(ResolvedType::value(TypeId::from_bytes(id::<71>()))),
    );
    assert_eq!(
        validate_resolved_type_slots(&standard_context(), &hostile_client,),
        Err(
            RevisionInvariantError::LegacyScalarRequiresCatalogueHashVersionOne {
                identity: DefinitionIdentity::Parameter {
                    owner: FunctionId::from_bytes(id::<82>()),
                    parameter: ParameterId::from_bytes(id::<83>()),
                },
                scalar: StandardScalar::Integer,
            },
        )
    );
}

#[test]
fn resolved_value_revision_errors_have_exact_source_free_contracts() {
    let identity = DefinitionIdentity::Function(FunctionId::from_bytes(id::<82>()));
    let value_type = TypeId::from_bytes(id::<71>());
    let cases = [
        (
            RevisionInvariantError::ResolvedValueRequiresCatalogueHashVersionTwo {
                identity,
                value_type,
            },
            "resolved value type requires catalogue hash version 2",
        ),
        (
            RevisionInvariantError::LegacyScalarRequiresCatalogueHashVersionOne {
                identity,
                scalar: StandardScalar::Boolean,
            },
            "legacy scalar resolved type requires catalogue hash version 1",
        ),
        (
            RevisionInvariantError::ResolvedValueTypeNotInPinnedStandard {
                identity,
                value_type,
            },
            "resolved value type is absent from the pinned standard library",
        ),
        (
            RevisionInvariantError::OpaqueValueTypeNotAcceptedInSlot {
                identity,
                value_type,
            },
            "opaque value type is not accepted in a catalogue slot",
        ),
    ];

    for (error, display) in cases {
        assert_eq!(error.to_string(), display);
        assert!(Error::source(&error).is_none());
    }
}

#[test]
fn catalogue_hash_context_errors_explain_the_required_version() {
    let function = FunctionId::from_bytes(id::<9>());
    let revision = FunctionRevisionId::from_bytes(id::<11>());
    let target = TypeId::from_bytes(id::<71>());
    let cases = [
        (
            RevisionInvariantError::ValueTypeDefinitionRequiresCatalogueHashVersionTwo {
                value_type: target,
            },
            "value types require catalogue hash version 2",
        ),
        (
            RevisionInvariantError::TypeBindingRequiresCatalogueHashVersionTwo {
                binding: TypeBindingId::from_bytes(id::<72>()),
            },
            "type-name bindings require catalogue hash version 2",
        ),
        (
            RevisionInvariantError::DefinitionOriginRequiresCatalogueHashVersionTwo {
                identity: DefinitionIdentity::ValueType(target),
            },
            "value-type and type-binding origins require catalogue hash version 2",
        ),
        (
            RevisionInvariantError::ValueTypeReferenceRequiresCatalogueHashVersionTwo {
                function,
                revision,
                target,
            },
            "value-type references require catalogue hash version 2",
        ),
        (
            RevisionInvariantError::FunctionSemanticHashVersionRequiresCatalogueHashVersionTwo {
                function,
                revision,
            },
            "function semantic hash version 2 requires catalogue hash version 2",
        ),
        (
            RevisionInvariantError::ValueTypeReferenceFunctionRevisionUnavailable {
                function,
                revision,
                target,
            },
            "cannot verify a value-type reference without its function revision record",
        ),
        (
            RevisionInvariantError::ValueTypeReferenceRequiresFunctionSemanticHashVersionTwo {
                function,
                revision,
                target,
            },
            "value-type references require function semantic hash version 2",
        ),
        (
            RevisionInvariantError::DeployableCurrentFunctionRevisionsRequired,
            "catalogue hash version 2 requires complete current function revision evidence",
        ),
        (
            RevisionInvariantError::MissingDeployableCurrentFunctionRevision { function, revision },
            "current function revision evidence is incomplete for the candidate",
        ),
        (
            RevisionInvariantError::NewFunctionRevisionCurrentEvidenceMismatch {
                function,
                revision,
            },
            "a new function revision does not match the supplied current revision evidence",
        ),
    ];

    for (error, message) in cases {
        assert_eq!(error.to_string(), message);
    }
}

#[test]
fn rejects_sparse_reference_ordinals_in_deployable_revision() {
    let expected = RevisionPair::new(
        SourceRevisionId::from_bytes(id::<20>()),
        CatalogueRevisionId::from_bytes(id::<21>()),
    );
    let revision = function_revision();
    let sparse_references = [0, 2]
        .into_iter()
        .map(|ordinal| {
            DefinitionReference::new(
                revision.function(),
                revision.id(),
                ordinal,
                DefinitionReferenceTarget::Function(revision.function()),
                DefinitionReferenceKind::FunctionCall,
                revision.declaration_origin(),
            )
        })
        .collect();

    assert_eq!(
        DeployableRevision::new(
            expected,
            source(Some(expected.source())),
            expected.catalogue(),
            function_catalogue(revision.id()),
            digest::<7>(),
            function_origins(&revision),
            vec![],
            vec![revision],
            sparse_references,
        )
        .unwrap_err(),
        RevisionInvariantError::ReferenceOrdinalOutOfSequence {
            revision: FunctionRevisionId::from_bytes(id::<11>()),
            expected: 1,
            actual: 2,
        }
    );

    let revision = function_revision();
    let leading_sparse_reference = DefinitionReference::new(
        revision.function(),
        revision.id(),
        1,
        DefinitionReferenceTarget::Function(revision.function()),
        DefinitionReferenceKind::FunctionCall,
        revision.declaration_origin(),
    );
    assert_eq!(
        DeployableRevision::new(
            expected,
            source(Some(expected.source())),
            expected.catalogue(),
            function_catalogue(revision.id()),
            digest::<7>(),
            function_origins(&revision),
            vec![],
            vec![revision],
            vec![leading_sparse_reference],
        )
        .unwrap_err(),
        RevisionInvariantError::ReferenceOrdinalOutOfSequence {
            revision: FunctionRevisionId::from_bytes(id::<11>()),
            expected: 0,
            actual: 1,
        }
    );
}

#[test]
fn rejects_sparse_reference_ordinals_in_active_revision() {
    let source = source(None);
    let revision = function_revision();
    let reference = DefinitionReference::new(
        revision.function(),
        revision.id(),
        1,
        DefinitionReferenceTarget::Function(revision.function()),
        DefinitionReferenceKind::FunctionCall,
        revision.declaration_origin(),
    );

    assert_eq!(
        ActiveDatabaseRevision::new(
            RevisionPair::new(source.id(), CatalogueRevisionId::from_bytes(id::<7>())),
            source,
            function_catalogue(revision.id()),
            digest::<7>(),
            vec![],
            vec![revision.clone()],
            function_origins(&revision),
            vec![reference],
        )
        .unwrap_err(),
        RevisionInvariantError::ReferenceOrdinalOutOfSequence {
            revision: revision.id(),
            expected: 0,
            actual: 1,
        }
    );
}

#[test]
fn accepts_interleaved_reference_ordinals_for_multiple_revisions() {
    let source = source(None);
    let function_a = FunctionId::from_bytes(id::<90>());
    let function_b = FunctionId::from_bytes(id::<91>());
    let revision_a = function_revision_fixture(
        function_a,
        FunctionRevisionId::from_bytes(id::<92>()),
        digest::<93>(),
        digest::<94>(),
    );
    let revision_b = function_revision_fixture(
        function_b,
        FunctionRevisionId::from_bytes(id::<95>()),
        digest::<96>(),
        digest::<97>(),
    );
    let catalogue = function_catalogue_with_functions(vec![
        function_definition_named(
            ["crm", "lookup_a"].as_slice(),
            function_a,
            revision_a.id(),
            ResolvedType::scalar(StandardScalar::Boolean),
        ),
        function_definition_named(
            ["crm", "lookup_b"].as_slice(),
            function_b,
            revision_b.id(),
            ResolvedType::scalar(StandardScalar::Boolean),
        ),
    ]);
    let mut origins = function_origins(&revision_a);
    origins.extend([
        DefinitionOrigin::new(
            DefinitionIdentity::Function(function_b),
            revision_b.declaration_origin(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::FunctionReturnColumn {
                owner: function_b,
                ordinal: 0,
            },
            revision_b.declaration_origin(),
        ),
    ]);
    let references = vec![
        DefinitionReference::new(
            function_a,
            revision_a.id(),
            0,
            DefinitionReferenceTarget::Function(function_b),
            DefinitionReferenceKind::FunctionCall,
            revision_a.declaration_origin(),
        ),
        DefinitionReference::new(
            function_b,
            revision_b.id(),
            0,
            DefinitionReferenceTarget::Function(function_a),
            DefinitionReferenceKind::FunctionCall,
            revision_b.declaration_origin(),
        ),
        DefinitionReference::new(
            function_a,
            revision_a.id(),
            1,
            DefinitionReferenceTarget::Function(function_b),
            DefinitionReferenceKind::FunctionCall,
            revision_a.declaration_origin(),
        ),
        DefinitionReference::new(
            function_b,
            revision_b.id(),
            1,
            DefinitionReferenceTarget::Function(function_a),
            DefinitionReferenceKind::FunctionCall,
            revision_b.declaration_origin(),
        ),
    ];

    assert!(
        ActiveDatabaseRevision::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            digest::<98>(),
            vec![],
            vec![revision_a, revision_b],
            origins,
            references,
        )
        .is_ok()
    );
}

#[test]
fn accepts_write_references_and_rejects_crossed_or_other_targets() {
    let expected = RevisionPair::new(
        SourceRevisionId::from_bytes(id::<20>()),
        CatalogueRevisionId::from_bytes(id::<21>()),
    );
    let revision = function_revision();
    let object_type = TypeId::from_bytes(id::<12>());
    let field = FieldId::from_bytes(id::<13>());
    let valid_references = vec![
        DefinitionReference::new(
            revision.function(),
            revision.id(),
            0,
            DefinitionReferenceTarget::ObjectType(object_type),
            DefinitionReferenceKind::WriteObject,
            revision.declaration_origin(),
        ),
        DefinitionReference::new(
            revision.function(),
            revision.id(),
            1,
            DefinitionReferenceTarget::Field {
                owner: object_type,
                field,
            },
            DefinitionReferenceKind::WriteField,
            revision.declaration_origin(),
        ),
    ];
    assert!(
        DeployableRevision::new(
            expected,
            source(Some(expected.source())),
            expected.catalogue(),
            write_catalogue(revision.id()),
            digest::<7>(),
            write_origins(&revision),
            vec![],
            vec![revision.clone()],
            valid_references,
        )
        .is_ok()
    );

    for (kind, target) in [
        (
            DefinitionReferenceKind::WriteObject,
            DefinitionReferenceTarget::Field {
                owner: object_type,
                field,
            },
        ),
        (
            DefinitionReferenceKind::WriteField,
            DefinitionReferenceTarget::ObjectType(object_type),
        ),
        (
            DefinitionReferenceKind::WriteObject,
            DefinitionReferenceTarget::Function(revision.function()),
        ),
        (
            DefinitionReferenceKind::WriteField,
            DefinitionReferenceTarget::Function(revision.function()),
        ),
    ] {
        let reference = DefinitionReference::new(
            revision.function(),
            revision.id(),
            0,
            target,
            kind,
            revision.declaration_origin(),
        );
        assert!(matches!(
            DeployableRevision::new(
                expected,
                source(Some(expected.source())),
                expected.catalogue(),
                write_catalogue(revision.id()),
                digest::<7>(),
                write_origins(&revision),
                vec![],
                vec![revision.clone()],
                vec![reference],
            ),
            Err(RevisionInvariantError::ReferenceKindTargetMismatch { .. })
        ));
    }
}
