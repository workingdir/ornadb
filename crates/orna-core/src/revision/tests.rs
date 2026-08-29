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

fn record_graph_standard() -> CatalogueSnapshot {
    CatalogueSnapshot::new(CatalogueRevisionId::from_bytes(id::<150>()), vec![], vec![]).unwrap()
}

fn record_graph_standard_with_boolean() -> CatalogueSnapshot {
    CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes(id::<151>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<152>()),
            QualifiedSemanticName::new(["std"]).unwrap(),
        )],
        vec![],
        vec![ValueTypeDefinition::primitive(
            TypeId::from_bytes(id::<71>()),
            QualifiedSemanticName::new(["std", "boolean"]).unwrap(),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.boolean@1",
        )],
        vec![],
    )
    .unwrap()
}

fn record_graph_schema() -> SchemaDefinition {
    SchemaDefinition::new(
        SchemaId::from_bytes(id::<8>()),
        QualifiedSemanticName::new(["crm"]).unwrap(),
    )
}

fn record_graph_type(
    record_byte: u8,
    name: &str,
    fields: Vec<(u8, u32, TypeId)>,
) -> RecordValueTypeDefinition {
    let fields = fields
        .into_iter()
        .map(|(field_byte, ordinal, target)| {
            RecordValueFieldDefinition::try_new_descriptor(
                FieldId::from_bytes([field_byte; 16]),
                format!("edge_{ordinal}"),
                ordinal,
                TypeDescriptor::named(target),
            )
            .unwrap()
        })
        .collect();
    RecordValueTypeDefinition::new(
        TypeId::from_bytes([record_byte; 16]),
        QualifiedSemanticName::new(["crm", name]).unwrap(),
        fields,
    )
}

fn record_graph_catalogue(records: Vec<RecordValueTypeDefinition>) -> CatalogueSnapshot {
    CatalogueSnapshot::new_with_record_value_types(
        CatalogueRevisionId::from_bytes(id::<152>()),
        vec![record_graph_schema()],
        vec![],
        vec![],
        vec![],
        records,
        vec![],
    )
    .unwrap()
}

fn record_graph_type_with_id(
    id: [u8; 16],
    name: &str,
    fields: Vec<([u8; 16], u32, TypeId)>,
) -> RecordValueTypeDefinition {
    let fields = fields
        .into_iter()
        .map(|(field_id, ordinal, target)| {
            RecordValueFieldDefinition::try_new_descriptor(
                FieldId::from_bytes(field_id),
                format!("edge_{ordinal}"),
                ordinal,
                TypeDescriptor::named(target),
            )
            .unwrap()
        })
        .collect();
    RecordValueTypeDefinition::new(
        TypeId::from_bytes(id),
        QualifiedSemanticName::new(["crm", name]).unwrap(),
        fields,
    )
}

/// A deterministic record identity for one chain index beyond one byte.
fn index_type_id(index: u32) -> [u8; 16] {
    let value = index + 0x1000;
    let mut bytes = [0; 16];
    bytes[0] = (value >> 8) as u8;
    bytes[1] = value as u8;
    bytes
}

/// A deterministic field identity for one chain index beyond one byte.
fn index_field_id(index: u32) -> [u8; 16] {
    let mut bytes = index_type_id(index);
    bytes[15] = 0x01;
    bytes
}

fn record_graph_origins_for(catalogue: &CatalogueSnapshot) -> Vec<DefinitionOrigin> {
    let source_unit = SourceUnitId::from_bytes(id::<3>());
    let mut identities = vec![DefinitionIdentity::Schema(SchemaId::from_bytes(id::<8>()))];
    for record in catalogue.record_value_types() {
        identities.push(DefinitionIdentity::ValueType(record.id()));
        for field in record.fields() {
            identities.push(DefinitionIdentity::Field {
                owner: record.id(),
                field: field.id(),
            });
        }
    }
    identities
        .into_iter()
        .enumerate()
        .map(|(index, identity)| {
            DefinitionOrigin::new(
                identity,
                SourceOrigin::new(source_unit, index as u32, index as u32 + 1).unwrap(),
            )
        })
        .collect()
}

fn validate_record_graph(
    records: Vec<RecordValueTypeDefinition>,
) -> Result<(), RecordValueFieldDescriptorValidationError> {
    validate_record_value_field_descriptors(
        &record_graph_catalogue(records),
        &record_graph_standard_with_boolean(),
    )
}

#[test]
fn record_value_field_application_record_provenance_and_active_projection() {
    let record_a = record_graph_type(
        200,
        "record_a",
        vec![(210, 0, TypeId::from_bytes(id::<201>()))],
    );
    let record_b = record_graph_type(
        201,
        "record_b",
        vec![(211, 0, TypeId::from_bytes(id::<71>()))],
    );
    let catalogue = record_graph_catalogue(vec![record_a.clone(), record_b.clone()]);

    assert_eq!(
        classify_record_value_field_descriptor(
            &catalogue,
            &record_graph_standard(),
            &TypeDescriptor::named(TypeId::from_bytes(id::<200>())),
        ),
        Ok(RecordValueFieldDescriptorClass::ApplicationRecord(
            TypeId::from_bytes(id::<200>())
        ))
    );
    assert_eq!(
        classify_record_value_field_descriptor(
            &catalogue,
            &record_graph_standard(),
            &TypeDescriptor::named(TypeId::from_bytes(id::<201>())),
        ),
        Ok(RecordValueFieldDescriptorClass::ApplicationRecord(
            TypeId::from_bytes(id::<201>())
        ))
    );
    assert_eq!(
        classify_record_value_field_descriptor(
            &catalogue,
            &record_graph_standard(),
            &TypeDescriptor::reference(TypeId::from_bytes(id::<200>())),
        ),
        Err(RecordValueFieldDescriptorClassificationError::Unsupported)
    );

    let active = active_for_flat_type_conversion(catalogue, standard_context());
    assert_eq!(
        active.record_value_field_descriptor_runtime_type(&TypeDescriptor::named(
            TypeId::from_bytes(id::<200>())
        )),
        Some(ResolvedType::named(TypeId::from_bytes(id::<200>())))
    );
    assert_eq!(
        active.record_value_field_descriptor_runtime_type(&TypeDescriptor::named(
            TypeId::from_bytes(id::<71>())
        )),
        Some(ResolvedType::scalar(StandardScalar::Boolean))
    );
    assert_eq!(
        active.record_value_field_descriptor_runtime_type(&TypeDescriptor::reference(
            TypeId::from_bytes(id::<200>())
        )),
        None
    );
    assert_eq!(validate_record_graph(vec![record_a, record_b]), Ok(()));
}

#[test]
fn record_value_field_self_cycle_is_rejected_exactly() {
    let record_a = record_graph_type(
        200,
        "record_a",
        vec![(210, 0, TypeId::from_bytes(id::<200>()))],
    );
    assert_eq!(
        validate_record_graph(vec![record_a]).unwrap_err(),
        RecordValueFieldDescriptorValidationError::RecursiveRecordValueField {
            record_value_type: TypeId::from_bytes(id::<200>()),
            field: FieldId::from_bytes(id::<210>()),
            nested_record_value_type: TypeId::from_bytes(id::<200>()),
        }
    );
}

#[test]
fn record_value_field_three_cycle_closes_at_the_exact_back_edge() {
    let record_a = record_graph_type(
        200,
        "record_a",
        vec![(210, 0, TypeId::from_bytes(id::<201>()))],
    );
    let record_b = record_graph_type(
        201,
        "record_b",
        vec![(211, 0, TypeId::from_bytes(id::<202>()))],
    );
    let record_c = record_graph_type(
        202,
        "record_c",
        vec![(212, 0, TypeId::from_bytes(id::<200>()))],
    );
    assert_eq!(
        validate_record_graph(vec![record_a, record_b, record_c]).unwrap_err(),
        RecordValueFieldDescriptorValidationError::RecursiveRecordValueField {
            record_value_type: TypeId::from_bytes(id::<202>()),
            field: FieldId::from_bytes(id::<212>()),
            nested_record_value_type: TypeId::from_bytes(id::<200>()),
        }
    );
}

#[test]
fn record_value_field_cycle_selection_is_deterministic_across_orders() {
    // The A-B-C-A cycle must report the same closing edge when the record
    // input order is reversed.
    let forward = vec![
        record_graph_type(
            200,
            "record_a",
            vec![(210, 0, TypeId::from_bytes(id::<201>()))],
        ),
        record_graph_type(
            201,
            "record_b",
            vec![(211, 0, TypeId::from_bytes(id::<202>()))],
        ),
        record_graph_type(
            202,
            "record_c",
            vec![(212, 0, TypeId::from_bytes(id::<200>()))],
        ),
    ];
    let reversed = forward.iter().rev().cloned().collect::<Vec<_>>();
    let expected = RecordValueFieldDescriptorValidationError::RecursiveRecordValueField {
        record_value_type: TypeId::from_bytes(id::<202>()),
        field: FieldId::from_bytes(id::<212>()),
        nested_record_value_type: TypeId::from_bytes(id::<200>()),
    };
    assert_eq!(validate_record_graph(forward).unwrap_err(), expected);
    assert_eq!(validate_record_graph(reversed).unwrap_err(), expected);

    // The closing edge is selected by ordinal, not by input field order.
    let first_field_first = vec![
        record_graph_type(
            200,
            "record_a",
            vec![
                (210, 0, TypeId::from_bytes(id::<201>())),
                (214, 1, TypeId::from_bytes(id::<202>())),
            ],
        ),
        record_graph_type(
            201,
            "record_b",
            vec![(211, 0, TypeId::from_bytes(id::<200>()))],
        ),
        record_graph_type(
            202,
            "record_c",
            vec![(212, 0, TypeId::from_bytes(id::<71>()))],
        ),
    ];
    assert_eq!(
        validate_record_graph(first_field_first).unwrap_err(),
        RecordValueFieldDescriptorValidationError::RecursiveRecordValueField {
            record_value_type: TypeId::from_bytes(id::<201>()),
            field: FieldId::from_bytes(id::<211>()),
            nested_record_value_type: TypeId::from_bytes(id::<200>()),
        }
    );
    let second_field_first = vec![
        record_graph_type(
            200,
            "record_a",
            vec![
                (214, 0, TypeId::from_bytes(id::<201>())),
                (210, 1, TypeId::from_bytes(id::<202>())),
            ],
        ),
        record_graph_type(
            201,
            "record_b",
            vec![(211, 0, TypeId::from_bytes(id::<200>()))],
        ),
        record_graph_type(
            202,
            "record_c",
            vec![(212, 0, TypeId::from_bytes(id::<71>()))],
        ),
    ];
    assert_eq!(
        validate_record_graph(second_field_first).unwrap_err(),
        RecordValueFieldDescriptorValidationError::RecursiveRecordValueField {
            record_value_type: TypeId::from_bytes(id::<201>()),
            field: FieldId::from_bytes(id::<211>()),
            nested_record_value_type: TypeId::from_bytes(id::<200>()),
        }
    );
}

#[test]
fn record_value_field_diamond_dag_is_accepted() {
    let records = vec![
        record_graph_type(
            200,
            "record_a",
            vec![
                (210, 0, TypeId::from_bytes(id::<201>())),
                (214, 1, TypeId::from_bytes(id::<202>())),
            ],
        ),
        record_graph_type(
            201,
            "record_b",
            vec![(211, 0, TypeId::from_bytes(id::<203>()))],
        ),
        record_graph_type(
            202,
            "record_c",
            vec![(212, 0, TypeId::from_bytes(id::<203>()))],
        ),
        record_graph_type(
            203,
            "record_d",
            vec![(213, 0, TypeId::from_bytes(id::<71>()))],
        ),
    ];
    assert_eq!(validate_record_graph(records), Ok(()));
}

#[test]
fn record_value_field_classification_errors_precede_cycles() {
    // The application catalogue also defines a record at the standard
    // boolean identity, so record_a's first field is ambiguous, while the
    // record_a -> record_b -> record_a cycle exists. Classification runs
    // before cycle detection, so the ambiguous field must win.
    let colliding = record_graph_type(
        71,
        "record_71",
        vec![(215, 0, TypeId::from_bytes(id::<201>()))],
    );
    let record_a = record_graph_type(
        200,
        "record_a",
        vec![
            (210, 0, TypeId::from_bytes(id::<71>())),
            (214, 1, TypeId::from_bytes(id::<201>())),
        ],
    );
    let record_b = record_graph_type(
        201,
        "record_b",
        vec![(211, 0, TypeId::from_bytes(id::<200>()))],
    );
    let catalogue = record_graph_catalogue(vec![colliding, record_a, record_b]);
    let error =
        validate_record_value_field_descriptors(&catalogue, &record_graph_standard_with_boolean())
            .unwrap_err();
    assert_eq!(
        error,
        RecordValueFieldDescriptorValidationError::Ambiguous {
            record_value_type: TypeId::from_bytes(id::<200>()),
            field: FieldId::from_bytes(id::<210>()),
            type_id: TypeId::from_bytes(id::<71>()),
        }
    );
}

#[test]
fn record_value_field_cycles_precede_depth() {
    let record_a = record_graph_type(
        200,
        "record_a",
        vec![
            (210, 0, TypeId::from_bytes(id::<201>())),
            (214, 1, TypeId::from_bytes(id::<202>())),
        ],
    );
    let record_b = record_graph_type(
        201,
        "record_b",
        vec![(211, 0, TypeId::from_bytes(id::<200>()))],
    );
    let chain = (0..40)
        .map(|index| {
            let record_byte = 202 + index as u8;
            let next_byte = if index == 39 {
                71
            } else {
                202 + index as u8 + 1
            };
            record_graph_type(
                record_byte,
                &format!("chain_{index}"),
                vec![(250, 0, TypeId::from_bytes([next_byte; 16]))],
            )
        })
        .collect::<Vec<_>>();
    let mut records = vec![record_a, record_b];
    records.extend(chain);
    assert_eq!(
        validate_record_graph(records).unwrap_err(),
        RecordValueFieldDescriptorValidationError::RecursiveRecordValueField {
            record_value_type: TypeId::from_bytes(id::<201>()),
            field: FieldId::from_bytes(id::<211>()),
            nested_record_value_type: TypeId::from_bytes(id::<200>()),
        }
    );
}

#[test]
fn record_value_field_nesting_accepts_32_edges_and_rejects_33_exactly() {
    let accepted = (0..33)
        .map(|index| {
            let next_byte = if index == 32 {
                71
            } else {
                200 + index as u8 + 1
            };
            record_graph_type(
                200 + index as u8,
                &format!("chain_{index}"),
                vec![(210 + index as u8, 0, TypeId::from_bytes([next_byte; 16]))],
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(validate_record_graph(accepted), Ok(()));

    let too_deep = (0..34)
        .map(|index| {
            let next_byte = if index == 33 {
                71
            } else {
                200 + index as u8 + 1
            };
            record_graph_type(
                200 + index as u8,
                &format!("chain_{index}"),
                vec![(210 + index as u8, 0, TypeId::from_bytes([next_byte; 16]))],
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        validate_record_graph(too_deep).unwrap_err(),
        RecordValueFieldDescriptorValidationError::RecordValueNestingTooDeep {
            record_value_type: TypeId::from_bytes(id::<232>()),
            field: FieldId::from_bytes(id::<242>()),
            nested_record_value_type: TypeId::from_bytes(id::<233>()),
            maximum: 32,
            actual: 33,
        }
    );
}

#[test]
fn record_value_field_long_acyclic_chain_returns_the_exact_depth_error_without_crashing() {
    let chain = (0..4096)
        .map(|index| {
            let next = if index == 4095 {
                TypeId::from_bytes([71; 16])
            } else {
                TypeId::from_bytes(index_type_id(index + 1))
            };
            record_graph_type_with_id(
                index_type_id(index),
                &format!("chain_{index}"),
                vec![(index_field_id(index), 0, next)],
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        validate_record_graph(chain).unwrap_err(),
        RecordValueFieldDescriptorValidationError::RecordValueNestingTooDeep {
            record_value_type: TypeId::from_bytes(index_type_id(32)),
            field: FieldId::from_bytes(index_field_id(32)),
            nested_record_value_type: TypeId::from_bytes(index_type_id(33)),
            maximum: 32,
            actual: 33,
        }
    );
}

#[test]
fn record_value_field_shared_suffix_memoisation_still_fails_on_a_later_deep_root() {
    let shallow_root = record_graph_type(
        2,
        "shallow_root",
        vec![(212, 0, TypeId::from_bytes([3; 16]))],
    );
    let suffix_leaf = record_graph_type(
        3,
        "suffix_leaf",
        vec![(213, 0, TypeId::from_bytes([71; 16]))],
    );
    let deep_root = record_graph_type(4, "deep_root", vec![(214, 0, TypeId::from_bytes([5; 16]))]);
    let deep_chain = (0..32)
        .map(|index| {
            let next = if index == 31 {
                TypeId::from_bytes([3; 16])
            } else {
                TypeId::from_bytes([5 + index as u8 + 1; 16])
            };
            record_graph_type(
                5 + index as u8,
                &format!("deep_{index}"),
                vec![(215 + index as u8, 0, next)],
            )
        })
        .collect::<Vec<_>>();
    let mut records = vec![shallow_root, suffix_leaf, deep_root];
    records.extend(deep_chain);
    assert_eq!(
        validate_record_graph(records).unwrap_err(),
        RecordValueFieldDescriptorValidationError::RecordValueNestingTooDeep {
            record_value_type: TypeId::from_bytes([36; 16]),
            field: FieldId::from_bytes([246; 16]),
            nested_record_value_type: TypeId::from_bytes([3; 16]),
            maximum: 32,
            actual: 33,
        }
    );
}

fn deployable_with(
    catalogue: CatalogueSnapshot,
    context: CatalogueHashContext,
) -> Result<DeployableRevision, RevisionInvariantError> {
    let expected_base = RevisionPair::new(
        SourceRevisionId::from_bytes(id::<78>()),
        CatalogueRevisionId::from_bytes(id::<79>()),
    );
    DeployableRevision::new_with_catalogue_hash_context(
        DeployableRevisionInput::new(
            expected_base,
            source(Some(expected_base.source())),
            expected_base.catalogue(),
            catalogue.clone(),
            digest::<7>(),
            DeployableRevisionContent::new(
                record_graph_origins_for(&catalogue),
                vec![],
                vec![],
                vec![],
            )
            .with_current_function_revisions(vec![]),
        ),
        context,
    )
}

#[test]
fn deployable_classification_returns_application_record_for_nested_targets() {
    let catalogue = record_graph_catalogue(vec![
        record_graph_type(
            200,
            "outer",
            vec![(210, 0, TypeId::from_bytes(id::<201>()))],
        ),
        record_graph_type(201, "inner", vec![(211, 0, TypeId::from_bytes(id::<71>()))]),
    ]);
    let deployable =
        deployable_with(catalogue, standard_context()).expect("nested record catalogue must admit");
    assert_eq!(
        deployable.record_value_field_descriptor_class(&TypeDescriptor::named(TypeId::from_bytes(
            id::<200>()
        ))),
        Ok(RecordValueFieldDescriptorClass::ApplicationRecord(
            TypeId::from_bytes(id::<200>())
        ))
    );
    assert_eq!(
        deployable.record_value_field_descriptor_class(&TypeDescriptor::named(TypeId::from_bytes(
            id::<201>()
        ))),
        Ok(RecordValueFieldDescriptorClass::ApplicationRecord(
            TypeId::from_bytes(id::<201>())
        ))
    );
}

#[test]
fn revision_admission_maps_cycles_and_depth_to_exact_errors() {
    let cyclic = record_graph_catalogue(vec![record_graph_type(
        200,
        "record_a",
        vec![(210, 0, TypeId::from_bytes([200; 16]))],
    )]);
    let expected_cycle = RevisionInvariantError::RecursiveRecordValueField {
        record_value_type: TypeId::from_bytes([200; 16]),
        field: FieldId::from_bytes([210; 16]),
        nested_record_value_type: TypeId::from_bytes([200; 16]),
    };
    let deployable_error = deployable_with(cyclic.clone(), standard_context())
        .expect_err("a self cycle must fail deployable admission");
    assert_eq!(deployable_error, expected_cycle);
    assert_eq!(
        deployable_error.to_string(),
        "record value fields must not form a recursive cycle"
    );

    let source = source(None);
    let pair = RevisionPair::new(source.id(), cyclic.revision());
    let active_origins = record_graph_origins_for(&cyclic);
    let active_error = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            pair,
            source,
            cyclic,
            digest::<7>(),
            ActiveRevisionContent::new(vec![], vec![], active_origins, vec![]),
        ),
        standard_context(),
    )
    .expect_err("a self cycle must fail active admission");
    assert_eq!(active_error, expected_cycle);

    let too_deep = record_graph_catalogue(
        (0..34)
            .map(|index| {
                let next_byte = if index == 33 {
                    71
                } else {
                    200 + index as u8 + 1
                };
                record_graph_type(
                    200 + index as u8,
                    &format!("chain_{index}"),
                    vec![(210 + index as u8, 0, TypeId::from_bytes([next_byte; 16]))],
                )
            })
            .collect(),
    );
    let expected_depth = RevisionInvariantError::RecordValueNestingTooDeep {
        record_value_type: TypeId::from_bytes([232; 16]),
        field: FieldId::from_bytes([242; 16]),
        nested_record_value_type: TypeId::from_bytes([233; 16]),
        maximum: 32,
        actual: 33,
    };
    let deployable_depth = deployable_with(too_deep.clone(), standard_context())
        .expect_err("a depth-33 chain must fail deployable admission");
    assert_eq!(deployable_depth, expected_depth);
    assert_eq!(
        deployable_depth.to_string(),
        "record value nesting exceeds the maximum depth"
    );
}

#[test]
fn application_record_colliding_with_a_standard_enum_is_ambiguous() {
    let candidate = record_graph_catalogue(vec![
        record_graph_type(
            75,
            "colliding",
            vec![(210, 0, TypeId::from_bytes([200; 16]))],
        ),
        record_graph_type(200, "leaf", vec![(211, 0, TypeId::from_bytes([71; 16]))]),
    ]);
    let deployable = deployable_with(candidate, flat_type_standard_context())
        .expect("colliding record catalogue must admit");
    assert_eq!(
        deployable.record_value_field_descriptor_class(&TypeDescriptor::named(TypeId::from_bytes(
            [75; 16]
        ))),
        Err(RecordValueFieldDescriptorError::Ambiguous {
            type_id: TypeId::from_bytes([75; 16]),
        })
    );

    let with_field = record_graph_catalogue(vec![
        record_graph_type(
            75,
            "colliding",
            vec![(211, 0, TypeId::from_bytes([71; 16]))],
        ),
        record_graph_type(200, "user", vec![(210, 0, TypeId::from_bytes([75; 16]))]),
    ]);
    let expected = RevisionInvariantError::AmbiguousRecordValueFieldType {
        record_value_type: TypeId::from_bytes([200; 16]),
        field: FieldId::from_bytes([210; 16]),
        type_id: TypeId::from_bytes([75; 16]),
    };
    let error = deployable_with(with_field, flat_type_standard_context())
        .expect_err("an ambiguous record field must fail admission");
    assert_eq!(error, expected);
    assert_eq!(
        error.to_string(),
        "record field type is present in both application and standard catalogues"
    );
}

fn catalogue_with_record_value_slot() -> CatalogueSnapshot {
    let record_value_type = TypeId::from_bytes(id::<76>());
    CatalogueSnapshot::new_with_functions_and_record_value_types(
        CatalogueRevisionId::from_bytes(id::<7>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<8>()),
            QualifiedSemanticName::new(["crm"]).unwrap(),
        )],
        vec![ObjectTypeDefinition::new(
            TypeId::from_bytes(id::<80>()),
            QualifiedSemanticName::new(["crm", "contact"]).unwrap(),
            vec![FieldDefinition::new(
                FieldId::from_bytes(id::<81>()),
                "status",
                0,
                ResolvedType::named(record_value_type),
                false,
                false,
                None,
                None,
            )],
        )],
        vec![],
        vec![],
        vec![RecordValueTypeDefinition::new(
            record_value_type,
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
        vec![FunctionDefinition::new(
            FunctionId::from_bytes(id::<82>()),
            QualifiedSemanticName::new(["crm", "read_status"]).unwrap(),
            FunctionDomain::Server,
            vec![],
            FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                "status",
                0,
                ResolvedType::named(record_value_type),
            )]),
            FunctionRevisionId::from_bytes(id::<83>()),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        )],
    )
    .unwrap()
}

fn record_value_type_origins() -> Vec<DefinitionOrigin> {
    let source = SourceUnitId::from_bytes(id::<3>());
    vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(SchemaId::from_bytes(id::<8>())),
            SourceOrigin::new(source, 0, 1).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(TypeId::from_bytes(id::<76>())),
            SourceOrigin::new(source, 1, 2).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Field {
                owner: TypeId::from_bytes(id::<76>()),
                field: FieldId::from_bytes(id::<77>()),
            },
            SourceOrigin::new(source, 2, 3).unwrap(),
        ),
    ]
}

#[test]
fn record_value_types_require_version_two_at_every_revision_admission_boundary() {
    let record_value_type = TypeId::from_bytes(id::<76>());
    let expected = RevisionInvariantError::RecordValueTypeRequiresCatalogueHashVersionTwo {
        record_value_type,
    };

    assert_eq!(
        validate_catalogue_hash_context_coherence(
            &CatalogueHashContext::version_one(),
            &record_value_type_catalogue(),
            &[],
            &[],
            &[],
        ),
        Err(expected.clone())
    );
    assert!(
        validate_catalogue_hash_context_coherence(
            &standard_context(),
            &record_value_type_catalogue(),
            &[],
            &[],
            &[],
        )
        .is_ok()
    );
    let version_two_source = source(None);
    let version_two_catalogue = record_value_type_catalogue();
    let version_two_pair =
        RevisionPair::new(version_two_source.id(), version_two_catalogue.revision());
    assert!(
        ActiveDatabaseRevision::new_with_catalogue_hash_context(
            ActiveDatabaseRevisionInput::new(
                version_two_pair,
                version_two_source,
                version_two_catalogue,
                digest::<7>(),
                ActiveRevisionContent::new(vec![], vec![], record_value_type_origins(), vec![]),
            ),
            standard_context(),
        )
        .is_ok()
    );

    let active_source = source(None);
    let catalogue = record_value_type_catalogue();
    let pair = RevisionPair::new(active_source.id(), catalogue.revision());
    assert_eq!(
        ActiveDatabaseRevision::new(
            pair,
            active_source,
            catalogue,
            digest::<7>(),
            vec![],
            vec![],
            vec![],
            vec![],
        )
        .unwrap_err(),
        expected.clone()
    );

    let expected_base = RevisionPair::new(
        SourceRevisionId::from_bytes(id::<78>()),
        CatalogueRevisionId::from_bytes(id::<79>()),
    );
    let deployable = DeployableRevision::new_with_catalogue_hash_context(
        DeployableRevisionInput::new(
            expected_base,
            source(Some(expected_base.source())),
            expected_base.catalogue(),
            record_value_type_catalogue(),
            digest::<7>(),
            DeployableRevisionContent::new(record_value_type_origins(), vec![], vec![], vec![])
                .with_current_function_revisions(vec![]),
        ),
        standard_context(),
    )
    .unwrap();
    assert_eq!(
        deployable.record_value_field_descriptor_class(&TypeDescriptor::named(TypeId::from_bytes(
            id::<71>()
        ),)),
        Ok(RecordValueFieldDescriptorClass::StandardPrimitive(
            TypeId::from_bytes(id::<71>()),
        ))
    );
    assert_eq!(
        deployable.record_value_field_descriptor_class(&TypeDescriptor::reference(
            TypeId::from_bytes(id::<80>()),
        )),
        Err(RecordValueFieldDescriptorError::Unsupported)
    );
    let version_one = DeployableRevision::new(
        expected_base,
        source(Some(expected_base.source())),
        expected_base.catalogue(),
        empty_catalogue(),
        digest::<7>(),
        vec![],
        vec![],
        vec![],
        vec![],
    )
    .unwrap();
    assert_eq!(
        version_one.record_value_field_descriptor_class(&TypeDescriptor::named(
            TypeId::from_bytes(id::<71>()),
        )),
        Err(RecordValueFieldDescriptorError::StandardLibraryUnavailable)
    );
    let collision = DeployableRevision::new_with_catalogue_hash_context(
        DeployableRevisionInput::new(
            expected_base,
            source(Some(expected_base.source())),
            expected_base.catalogue(),
            enum_type_catalogue(),
            digest::<7>(),
            DeployableRevisionContent::new(value_type_origins(), vec![], vec![], vec![])
                .with_current_function_revisions(vec![]),
        ),
        standard_context(),
    )
    .unwrap();
    assert_eq!(
        collision.record_value_field_descriptor_class(&TypeDescriptor::named(TypeId::from_bytes(
            id::<71>()
        ),)),
        Err(RecordValueFieldDescriptorError::Ambiguous {
            type_id: TypeId::from_bytes(id::<71>()),
        })
    );
    let standard_enum = DeployableRevision::new_with_catalogue_hash_context(
        DeployableRevisionInput::new(
            expected_base,
            source(Some(expected_base.source())),
            expected_base.catalogue(),
            empty_catalogue(),
            digest::<7>(),
            DeployableRevisionContent::new(vec![], vec![], vec![], vec![])
                .with_current_function_revisions(vec![]),
        ),
        flat_type_standard_context(),
    )
    .unwrap();
    assert_eq!(
        standard_enum.record_value_field_descriptor_class(&TypeDescriptor::named(
            TypeId::from_bytes(id::<75>()),
        )),
        Ok(RecordValueFieldDescriptorClass::StandardEnum(
            TypeId::from_bytes(id::<75>()),
        ))
    );
    assert_eq!(
        DeployableRevision::new(
            expected_base,
            source(Some(expected_base.source())),
            expected_base.catalogue(),
            record_value_type_catalogue(),
            digest::<7>(),
            vec![],
            vec![],
            vec![],
            vec![],
        )
        .unwrap_err(),
        expected.clone()
    );

    let standard_revision = StandardLibraryRevisionId::from_bytes(id::<74>());
    assert_eq!(
        StandardLibrarySnapshot::new(
            standard_revision,
            StandardLibraryDigestVersion::Version1,
            source(None),
            "orna.language/1",
            record_value_type_catalogue(),
            vec![],
            digest::<75>(),
        )
        .unwrap_err(),
        RevisionInvariantError::UnsupportedStandardLibraryDefinition {
            revision: standard_revision,
        }
    );
    assert_eq!(
        expected.to_string(),
        "record value types require catalogue hash version 2"
    );
    let identities = expected_definition_identities(&record_value_type_catalogue(), &[]);
    assert!(identities.contains(&DefinitionIdentity::ValueType(record_value_type)));
    assert!(identities.contains(&DefinitionIdentity::Field {
        owner: record_value_type,
        field: FieldId::from_bytes(id::<77>()),
    }));
}

#[test]
fn record_value_types_can_enter_object_and_rows_slots() {
    assert!(
        validate_resolved_type_slots(&standard_context(), &catalogue_with_record_value_slot(),)
            .is_ok()
    );
}

#[test]
fn record_value_field_type_policy_is_closed_and_uses_pinned_standard_primitives() {
    let accepted_contracts = [
        ("orna.kernel.value.boolean@1", StandardScalar::Boolean),
        ("orna.kernel.value.integer@1", StandardScalar::Integer),
        ("orna.kernel.value.bigint@1", StandardScalar::BigInt),
        ("orna.kernel.value.float@1", StandardScalar::Float),
        (
            "orna.kernel.value.character-large-object@1",
            StandardScalar::CharacterLargeObject,
        ),
        (
            "orna.kernel.value.binary-large-object@1",
            StandardScalar::BinaryLargeObject,
        ),
    ];
    let accepted_values = accepted_contracts
        .iter()
        .enumerate()
        .map(|(index, (contract, _))| {
            ValueTypeDefinition::primitive(
                TypeId::from_bytes([index as u8 + 1; 16]),
                QualifiedSemanticName::new(["std", "types", *contract]).unwrap(),
                ValueTypeMutability::Immutable,
                ValueTypePersistence::Persistable,
                *contract,
            )
        })
        .collect::<Vec<_>>();
    let standard = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes(id::<90>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<91>()),
            QualifiedSemanticName::new(["std", "types"]).unwrap(),
        )],
        vec![],
        accepted_values,
        vec![],
    )
    .unwrap();
    let application = empty_catalogue();
    for (index, (_, scalar)) in accepted_contracts.iter().enumerate() {
        let resolved_type = ResolvedType::value(TypeId::from_bytes([index as u8 + 1; 16]));
        assert!(record_value_field_runtime_type(&application, &standard, resolved_type).is_some());
        assert_eq!(
            record_value_field_runtime_type(&application, &standard, resolved_type),
            Some(ResolvedType::scalar(*scalar))
        );
    }

    for contract in [
        "orna.kernel.value.decimal@1",
        "orna.kernel.value.uuid@1",
        "orna.kernel.value.date@1",
        "orna.kernel.value.time@1",
        "orna.kernel.value.timestamp@1",
        "orna.kernel.value.duration@1",
        "orna.kernel.value.void@1",
        "orna.kernel.value.custom@1",
    ] {
        assert!(
            accepted_record_scalar(&ValueTypeDefinition::primitive(
                TypeId::from_bytes(id::<92>()),
                QualifiedSemanticName::new(["std", "types", "excluded"]).unwrap(),
                ValueTypeMutability::Immutable,
                ValueTypePersistence::Persistable,
                contract,
            ))
            .is_none()
        );
    }

    let application_primitive = TypeId::from_bytes(id::<93>());
    let application_enum = TypeId::from_bytes(id::<94>());
    let standard_enum = TypeId::from_bytes(id::<95>());
    let application = CatalogueSnapshot::new_with_enum_types(
        CatalogueRevisionId::from_bytes(id::<96>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<97>()),
            QualifiedSemanticName::new(["app"]).unwrap(),
        )],
        vec![],
        vec![ValueTypeDefinition::primitive(
            application_primitive,
            QualifiedSemanticName::new(["app", "flag"]).unwrap(),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.boolean@1",
        )],
        vec![EnumTypeDefinition::new(
            application_enum,
            QualifiedSemanticName::new(["app", "phase"]).unwrap(),
            ["new"],
        )],
        vec![],
    )
    .unwrap();
    let standard = CatalogueSnapshot::new_with_enum_types(
        CatalogueRevisionId::from_bytes(id::<98>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<99>()),
            QualifiedSemanticName::new(["std"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![EnumTypeDefinition::new(
            standard_enum,
            QualifiedSemanticName::new(["std", "phase"]).unwrap(),
            ["new"],
        )],
        vec![],
    )
    .unwrap();
    assert!(
        record_value_field_runtime_type(
            &application,
            &standard,
            ResolvedType::value(application_primitive),
        )
        .is_none()
    );
    assert!(
        record_value_field_runtime_type(
            &application,
            &standard,
            ResolvedType::named(application_enum),
        )
        .is_some()
    );
    assert_eq!(
        classify_record_value_field_descriptor(
            &application,
            &standard,
            &TypeDescriptor::named(application_enum),
        ),
        Ok(RecordValueFieldDescriptorClass::ApplicationEnum(
            application_enum,
        ))
    );
    assert_eq!(
        classify_record_value_field_descriptor(
            &application,
            &standard,
            &TypeDescriptor::named(standard_enum),
        ),
        Ok(RecordValueFieldDescriptorClass::StandardEnum(standard_enum))
    );
    let colliding_standard = CatalogueSnapshot::new_with_enum_types(
        CatalogueRevisionId::from_bytes(id::<103>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<104>()),
            QualifiedSemanticName::new(["std"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![EnumTypeDefinition::new(
            application_enum,
            QualifiedSemanticName::new(["std", "phase"]).unwrap(),
            ["new"],
        )],
        vec![],
    )
    .unwrap();
    assert_eq!(
        classify_record_value_field_descriptor(
            &application,
            &colliding_standard,
            &TypeDescriptor::named(application_enum),
        ),
        Err(RecordValueFieldDescriptorClassificationError::Ambiguous {
            type_id: application_enum,
        })
    );
    assert!(
        record_value_field_runtime_type(
            &application,
            &standard,
            ResolvedType::named(standard_enum),
        )
        .is_some()
    );
    for unsupported in [
        ResolvedType::value(TypeId::from_bytes(id::<100>())),
        ResolvedType::named(TypeId::from_bytes(id::<101>())),
        ResolvedType::scalar(StandardScalar::Boolean),
        ResolvedType::reference(TypeId::from_bytes(id::<102>())),
    ] {
        assert!(record_value_field_runtime_type(&application, &standard, unsupported,).is_none());
    }

    let collision = TypeId::from_bytes(id::<71>());
    let collision_catalogue = CatalogueSnapshot::new_with_record_value_types(
        CatalogueRevisionId::from_bytes(id::<96>()),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes(id::<97>()),
            QualifiedSemanticName::new(["app"]).unwrap(),
        )],
        vec![],
        vec![],
        vec![EnumTypeDefinition::new(
            collision,
            QualifiedSemanticName::new(["app", "collision"]).unwrap(),
            ["value"],
        )],
        vec![RecordValueTypeDefinition::new(
            TypeId::from_bytes(id::<98>()),
            QualifiedSemanticName::new(["app", "record"]).unwrap(),
            vec![
                RecordValueFieldDefinition::try_new_descriptor(
                    FieldId::from_bytes(id::<99>()),
                    "value",
                    0,
                    TypeDescriptor::named(collision),
                )
                .unwrap(),
            ],
        )],
        vec![],
    )
    .unwrap();
    assert_eq!(
        validate_record_value_field_types(&standard_context(), &collision_catalogue),
        Err(RevisionInvariantError::AmbiguousRecordValueFieldType {
            record_value_type: TypeId::from_bytes(id::<98>()),
            field: FieldId::from_bytes(id::<99>()),
            type_id: collision,
        })
    );
    let error = RevisionInvariantError::AmbiguousRecordValueFieldType {
        record_value_type: TypeId::from_bytes(id::<98>()),
        field: FieldId::from_bytes(id::<99>()),
        type_id: collision,
    };
    assert_eq!(
        error.to_string(),
        "record field type is present in both application and standard catalogues"
    );
    assert!(std::error::Error::source(&error).is_none());

    let cases = [
        (
            RecordValueFieldDescriptorError::StandardLibraryUnavailable,
            "deployable revision has no pinned standard library for record field classification",
        ),
        (
            RecordValueFieldDescriptorError::Unsupported,
            "record field descriptor is not supported by the deployable revision",
        ),
        (
            RecordValueFieldDescriptorError::Ambiguous { type_id: collision },
            "record field type is present in both application and standard catalogues",
        ),
    ];
    for (error, expected) in cases {
        assert_eq!(error.to_string(), expected);
        assert!(std::error::Error::source(&error).is_none());
    }
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

fn invocation_carrier_value_type(id: TypeId, parts: &[&str]) -> ValueTypeDefinition {
    ValueTypeDefinition::primitive(
        id,
        QualifiedSemanticName::new(parts.iter().copied()).unwrap(),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.test.invocation-carrier@1",
    )
}

fn invocation_carrier_catalogue(value_types: Vec<ValueTypeDefinition>) -> CatalogueSnapshot {
    CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes(id::<112>()),
        vec![
            SchemaDefinition::new(
                SchemaId::from_bytes(id::<113>()),
                QualifiedSemanticName::new(["sys"]).unwrap(),
            ),
            SchemaDefinition::new(
                SchemaId::from_bytes(id::<114>()),
                QualifiedSemanticName::new(["sys", "invoke"]).unwrap(),
            ),
        ],
        vec![],
        value_types,
        vec![],
    )
    .unwrap()
}

fn invocation_carrier_origins(catalogue: &CatalogueSnapshot) -> Vec<DefinitionOrigin> {
    let source_unit = SourceUnitId::from_bytes(id::<3>());
    let mut origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(SchemaId::from_bytes(id::<113>())),
            SourceOrigin::new(source_unit, 0, 1).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(SchemaId::from_bytes(id::<114>())),
            SourceOrigin::new(source_unit, 1, 2).unwrap(),
        ),
    ];
    origins.extend(
        catalogue
            .value_types()
            .iter()
            .enumerate()
            .map(|(index, value_type)| {
                DefinitionOrigin::new(
                    DefinitionIdentity::ValueType(value_type.id()),
                    SourceOrigin::new(source_unit, index as u32 + 2, index as u32 + 3).unwrap(),
                )
            }),
    );
    origins
}

fn active_invocation_carrier_admission(
    catalogue: CatalogueSnapshot,
    origins: Vec<DefinitionOrigin>,
) -> Result<ActiveDatabaseRevision, RevisionInvariantError> {
    let source = source(None);
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            digest::<115>(),
            ActiveRevisionContent::new(vec![], vec![], origins, vec![]),
        ),
        standard_context(),
    )
}

fn deployable_invocation_carrier_admission(
    catalogue: CatalogueSnapshot,
    origins: Vec<DefinitionOrigin>,
) -> Result<DeployableRevision, RevisionInvariantError> {
    let expected_base = RevisionPair::new(
        SourceRevisionId::from_bytes(id::<117>()),
        CatalogueRevisionId::from_bytes(id::<118>()),
    );
    DeployableRevision::new_with_catalogue_hash_context(
        DeployableRevisionInput::new(
            expected_base,
            source(Some(expected_base.source())),
            expected_base.catalogue(),
            catalogue,
            digest::<115>(),
            DeployableRevisionContent::new(origins, vec![], vec![], vec![])
                .with_current_function_revisions(vec![]),
        ),
        standard_context(),
    )
}

fn standard_invocation_carrier_admission(
    catalogue: CatalogueSnapshot,
    origins: Vec<DefinitionOrigin>,
) -> Result<StandardLibrarySnapshot, RevisionInvariantError> {
    StandardLibrarySnapshot::new(
        StandardLibraryRevisionId::from_bytes(id::<115>()),
        StandardLibraryDigestVersion::Version1,
        source(None),
        "orna.language/1",
        catalogue,
        origins,
        digest::<115>(),
    )
}

#[test]
fn public_application_and_standard_admission_reject_each_reserved_carrier_identity() {
    for &carrier in crate::system::INVOCATION_CARRIERS {
        let catalogue = invocation_carrier_catalogue(vec![invocation_carrier_value_type(
            carrier.id(),
            carrier.name_parts(),
        )]);
        let expected = RevisionInvariantError::ReservedInvocationCarrierIdentity {
            carrier: carrier.id(),
        };

        assert_eq!(
            active_invocation_carrier_admission(catalogue.clone(), vec![]).unwrap_err(),
            expected
        );
        assert_eq!(
            deployable_invocation_carrier_admission(catalogue.clone(), vec![]).unwrap_err(),
            expected
        );
        assert_eq!(
            standard_invocation_carrier_admission(catalogue, vec![]).unwrap_err(),
            expected
        );
    }
}

#[test]
fn public_application_and_standard_admission_reject_each_reserved_carrier_name() {
    for (index, &carrier) in crate::system::INVOCATION_CARRIERS.iter().enumerate() {
        let type_id = TypeId::from_bytes([0x80 + index as u8; 16]);
        let catalogue = invocation_carrier_catalogue(vec![invocation_carrier_value_type(
            type_id,
            carrier.name_parts(),
        )]);
        let expected = RevisionInvariantError::ReservedInvocationCarrierName { type_id };

        assert_eq!(
            active_invocation_carrier_admission(catalogue.clone(), vec![]).unwrap_err(),
            expected
        );
        assert_eq!(
            deployable_invocation_carrier_admission(catalogue.clone(), vec![]).unwrap_err(),
            expected
        );
        assert_eq!(
            standard_invocation_carrier_admission(catalogue, vec![]).unwrap_err(),
            expected
        );
    }
}

#[test]
fn carrier_identities_globally_precede_carrier_names_at_public_application_admission() {
    let value_name_only = TypeId::from_bytes([0xa0; 16]);
    let catalogue = invocation_carrier_catalogue(vec![
        invocation_carrier_value_type(value_name_only, &["sys", "invoke", "Value"]),
        invocation_carrier_value_type(
            crate::system::SYS_INVOKE_EVENT_TYPE_ID,
            &["sys", "invoke", "Event2"],
        ),
        invocation_carrier_value_type(
            crate::system::SYS_INVOKE_REQUEST_TYPE_ID,
            &["sys", "invoke", "Request2"],
        ),
    ]);
    let expected = RevisionInvariantError::ReservedInvocationCarrierIdentity {
        carrier: crate::system::SYS_INVOKE_REQUEST_TYPE_ID,
    };

    assert_eq!(
        active_invocation_carrier_admission(catalogue.clone(), vec![]).unwrap_err(),
        expected
    );
    assert_eq!(
        deployable_invocation_carrier_admission(catalogue.clone(), vec![]).unwrap_err(),
        expected
    );
    assert_eq!(
        standard_invocation_carrier_admission(catalogue, vec![]).unwrap_err(),
        expected
    );
}

#[test]
fn carrier_names_use_the_global_registry_order_at_public_application_admission() {
    let catalogue = invocation_carrier_catalogue(vec![
        invocation_carrier_value_type(TypeId::from_bytes([0x90; 16]), &["sys", "invoke", "Event"]),
        invocation_carrier_value_type(
            TypeId::from_bytes([0x91; 16]),
            &["sys", "invoke", "Request"],
        ),
        invocation_carrier_value_type(TypeId::from_bytes([0x92; 16]), &["sys", "invoke", "Value"]),
    ]);
    let expected = RevisionInvariantError::ReservedInvocationCarrierName {
        type_id: TypeId::from_bytes([0x92; 16]),
    };

    assert_eq!(
        active_invocation_carrier_admission(catalogue.clone(), vec![]).unwrap_err(),
        expected
    );
    assert_eq!(
        deployable_invocation_carrier_admission(catalogue.clone(), vec![]).unwrap_err(),
        expected
    );
    assert_eq!(
        standard_invocation_carrier_admission(catalogue, vec![]).unwrap_err(),
        expected
    );
}

#[test]
fn neighbouring_invocation_carrier_names_remain_admissible_at_public_boundaries() {
    let catalogue = invocation_carrier_catalogue(vec![invocation_carrier_value_type(
        TypeId::from_bytes(id::<116>()),
        &["sys", "invoke", "Value2"],
    )]);
    let origins = invocation_carrier_origins(&catalogue);

    assert!(active_invocation_carrier_admission(catalogue.clone(), origins.clone()).is_ok());
    assert!(deployable_invocation_carrier_admission(catalogue.clone(), origins.clone()).is_ok());
    assert!(standard_invocation_carrier_admission(catalogue, origins).is_ok());
}

#[test]
fn active_and_deployable_revisions_reject_the_invoke_system_identity() {
    let function = crate::system::SYS_INVOKE_FUNCTION_ID;
    let revision = function_revision_fixture(
        function,
        FunctionRevisionId::from_bytes(id::<92>()),
        digest::<92>(),
        digest::<93>(),
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
fn active_and_deployable_revisions_reject_the_health_system_name() {
    let function = FunctionId::from_bytes([0x5a; 16]);
    let revision = function_revision_fixture(
        function,
        FunctionRevisionId::from_bytes(id::<94>()),
        digest::<94>(),
        digest::<95>(),
    );
    let catalogue = function_catalogue_with_functions(vec![function_definition_named(
        &["sys", "catalog", "health"],
        function,
        revision.id(),
        ResolvedType::scalar(StandardScalar::Boolean),
    )]);
    let origins = function_origins(&revision);
    let expected = RevisionInvariantError::ReservedSystemFunctionName { function };
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
        "the reserved system function name cannot enter an application catalogue"
    );
}

#[test]
fn active_and_deployable_revisions_reject_the_invoke_system_name() {
    let function = FunctionId::from_bytes([0x5b; 16]);
    let revision = function_revision_fixture(
        function,
        FunctionRevisionId::from_bytes(id::<96>()),
        digest::<96>(),
        digest::<97>(),
    );
    let catalogue = function_catalogue_with_functions(vec![function_definition_named(
        &["sys", "invoke"],
        function,
        revision.id(),
        ResolvedType::scalar(StandardScalar::Boolean),
    )]);
    let origins = function_origins(&revision);
    let expected = RevisionInvariantError::ReservedSystemFunctionName { function };
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
        "the reserved system function name cannot enter an application catalogue"
    );
}

#[test]
fn identity_collisions_precede_name_collisions_in_registry_order() {
    let name_collision = FunctionId::from_bytes([0x5c; 16]);
    let invoke_identity = crate::system::SYS_INVOKE_FUNCTION_ID;
    let health_identity = crate::security::CATALOGUE_HEALTH_FUNCTION_ID;
    let name_revision = function_revision_fixture(
        name_collision,
        FunctionRevisionId::from_bytes(id::<98>()),
        digest::<98>(),
        digest::<99>(),
    );
    let invoke_revision = function_revision_fixture(
        invoke_identity,
        FunctionRevisionId::from_bytes(id::<100>()),
        digest::<100>(),
        digest::<101>(),
    );
    let health_revision = function_revision_fixture(
        health_identity,
        FunctionRevisionId::from_bytes(id::<102>()),
        digest::<102>(),
        digest::<103>(),
    );
    // Reversed application definition input: the reserved name and the
    // invocation identity appear before the health identity. Admission
    // must still select the health identity collision because every
    // identity collision precedes every name collision and the registry
    // order is health then invocation.
    let catalogue = function_catalogue_with_functions(vec![
        function_definition_named(
            &["sys", "invoke"],
            name_collision,
            name_revision.id(),
            ResolvedType::scalar(StandardScalar::Boolean),
        ),
        function_definition_named(
            &["sys", "lookup"],
            invoke_identity,
            invoke_revision.id(),
            ResolvedType::scalar(StandardScalar::Boolean),
        ),
        function_definition_named(
            &["sys", "probe"],
            health_identity,
            health_revision.id(),
            ResolvedType::scalar(StandardScalar::Boolean),
        ),
    ]);
    let mut origins = function_origins(&name_revision);
    origins.extend(function_origins(&invoke_revision));
    origins.extend(function_origins(&health_revision));
    let expected = RevisionInvariantError::ReservedSystemFunctionIdentity {
        function: health_identity,
    };
    let active_source = source(None);

    let active_error = ActiveDatabaseRevision::new(
        RevisionPair::new(active_source.id(), catalogue.revision()),
        active_source,
        catalogue.clone(),
        digest::<7>(),
        vec![],
        vec![
            name_revision.clone(),
            invoke_revision.clone(),
            health_revision.clone(),
        ],
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
        vec![name_revision, invoke_revision, health_revision],
        vec![],
    )
    .unwrap_err();
    assert_eq!(deployable_error, expected);
}

#[test]
fn reserved_invoke_identity_beats_a_health_name_collision_in_one_definition() {
    let function_id = crate::system::SYS_INVOKE_FUNCTION_ID;
    let revision = function_revision_fixture(
        function_id,
        FunctionRevisionId::from_bytes(id::<106>()),
        digest::<106>(),
        digest::<107>(),
    );
    // One application definition carries the invocation identity and the
    // health function's exact name. The identity phase is global and runs
    // before the name phase, so admission must report the invocation
    // identity collision even though the same definition also collides
    // with the health name.
    let catalogue = function_catalogue_with_functions(vec![function_definition_named(
        &["sys", "catalog", "health"],
        function_id,
        revision.id(),
        ResolvedType::scalar(StandardScalar::Boolean),
    )]);
    let origins = function_origins(&revision);
    let expected = RevisionInvariantError::ReservedSystemFunctionIdentity {
        function: function_id,
    };
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
fn name_collisions_use_registry_order_independent_of_application_vector_order() {
    let invoke_name_id = FunctionId::from_bytes([0x5e; 16]);
    let health_name_id = FunctionId::from_bytes([0x5f; 16]);
    let invoke_name_revision = function_revision_fixture(
        invoke_name_id,
        FunctionRevisionId::from_bytes(id::<108>()),
        digest::<108>(),
        digest::<109>(),
    );
    let health_name_revision = function_revision_fixture(
        health_name_id,
        FunctionRevisionId::from_bytes(id::<110>()),
        digest::<110>(),
        digest::<111>(),
    );
    // Reversed application definition input: sys.invoke appears before
    // sys.catalog.health. Name collisions must follow registry order, so
    // admission reports the health-name collision from registry position
    // zero even though the invoke-name definition comes first in the
    // application vector. Both schemas are declared exactly because each
    // function must resolve against its own parent namespace.
    let schema_sys = SchemaDefinition::new(
        SchemaId::from_bytes(id::<8>()),
        QualifiedSemanticName::new(["sys"]).unwrap(),
    );
    let schema_sys_catalog = SchemaDefinition::new(
        SchemaId::from_bytes(id::<9>()),
        QualifiedSemanticName::new(["sys", "catalog"]).unwrap(),
    );
    let catalogue = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes(id::<7>()),
        vec![schema_sys, schema_sys_catalog],
        vec![],
        vec![
            function_definition_named(
                &["sys", "invoke"],
                invoke_name_id,
                invoke_name_revision.id(),
                ResolvedType::scalar(StandardScalar::Boolean),
            ),
            function_definition_named(
                &["sys", "catalog", "health"],
                health_name_id,
                health_name_revision.id(),
                ResolvedType::scalar(StandardScalar::Boolean),
            ),
        ],
    )
    .unwrap();
    // Reserved-name validation is authoritative before origin
    // completeness, so the failing revision's origin fixture suffices.
    let origins = function_origins(&health_name_revision);
    let expected = RevisionInvariantError::ReservedSystemFunctionName {
        function: health_name_id,
    };
    let active_source = source(None);

    let active_error = ActiveDatabaseRevision::new(
        RevisionPair::new(active_source.id(), catalogue.revision()),
        active_source,
        catalogue.clone(),
        digest::<7>(),
        vec![],
        vec![invoke_name_revision.clone(), health_name_revision.clone()],
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
        vec![invoke_name_revision, health_name_revision],
        vec![],
    )
    .unwrap_err();
    assert_eq!(deployable_error, expected);
    assert_eq!(
        deployable_error.to_string(),
        "the reserved system function name cannot enter an application catalogue"
    );
}

#[test]
fn neighbouring_sys_names_remain_admissible() {
    let function = FunctionId::from_bytes([0x5d; 16]);
    let revision = function_revision_fixture(
        function,
        FunctionRevisionId::from_bytes(id::<104>()),
        digest::<104>(),
        digest::<105>(),
    );
    let catalogue = function_catalogue_with_functions(vec![function_definition_named(
        &["sys", "probe"],
        function,
        revision.id(),
        ResolvedType::scalar(StandardScalar::Boolean),
    )]);
    let origins = function_origins(&revision);
    let active_source = source(None);
    ActiveDatabaseRevision::new(
        RevisionPair::new(active_source.id(), catalogue.revision()),
        active_source,
        catalogue,
        digest::<7>(),
        vec![],
        vec![revision],
        origins,
        vec![],
    )
    .expect("a neighbouring sys.probe name must remain admissible");
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
