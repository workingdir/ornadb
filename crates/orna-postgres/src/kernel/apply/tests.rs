
use orna_core::{
    CatalogueRevisionId, ExpressionId, FieldId, FunctionId, FunctionRevisionId, ParameterId,
    SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId, StandardLibraryRevisionId, TypeId,
    canonical_hash::{
        catalogue_digest_with_context, source_bundle_digest, source_revision_record_digest,
        source_unit_content_digest, verify_standard_library_snapshot,
    },
    catalogue::{
        CatalogueSnapshot, EnumTypeDefinition, FieldDefinition, FunctionDefinition, FunctionDomain,
        FunctionReturn, FunctionSecurity, FunctionTransaction, FunctionVolatility,
        ObjectTypeDefinition, QualifiedSemanticName, RecordValueFieldDefinition,
        RecordValueTypeDefinition, SchemaDefinition, ValueTypeKind,
    },
    physical::{PhysicalPlanError, plan_physical_changes},
    revision::{
        ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
        CatalogueHashContext, DefinitionIdentity, DefinitionOrigin, DefinitionReferenceKind,
        DefinitionReferenceTarget, DeployableRevision, DeployableRevisionContent,
        DeployableRevisionInput, ExecutableArtifact, ExecutableArtifactKind,
        FunctionRevisionRecord, RevisionPair, Sha256Digest, SourceOrigin, StandardExecutable,
        StandardLibraryDigestVersion, StandardLibrarySnapshot, StoredSourceRevision,
        StoredSourceUnit,
    },
    types::{ResolvedType, StandardScalar, TypeDescriptor},
};

use super::reserved_identities::{
    StandardExecutableIdentity, StandardExecutableParameter, first_active_reserved_identity,
    first_active_standard_executable_identity, first_active_standard_parameter,
    first_inactive_reserved_identity, first_inactive_standard_executable_identity,
};
use super::{
    CandidateEncoder, LegacyTypeColumns, POSTGRES_REFERENCE_KINDS, StandardContextIdentity,
    TypeColumns, artifact_kind, function_transaction, guard_standard_context_transition,
    legacy_type_projection, materialize, positive_i32, positive_i64, reference_kind,
    reference_target, scalar, standard_reference_target_columns, standard_resolved_type_columns,
    standard_value_kind, type_columns, validate_candidate_preflight, validate_expected_base,
    validate_standard_executable_facts,
};
use crate::PostgresKernelError;

#[derive(Clone, Copy)]
struct StandardContextFixture {
    source_unit: [u8; 16],
    source_bundle: [u8; 16],
    source_revision: [u8; 16],
    standard_revision: [u8; 16],
    catalogue_revision: [u8; 16],
    logical_path: &'static str,
    content: &'static str,
    source_bundle_hash: [u8; 32],
    source_revision_hash: [u8; 32],
    standard_digest: [u8; 32],
}

const BASE_STANDARD_CONTEXT: StandardContextFixture = StandardContextFixture {
    source_unit: [4; 16],
    source_bundle: [5; 16],
    source_revision: [6; 16],
    standard_revision: [7; 16],
    catalogue_revision: [8; 16],
    logical_path: "std/malformed.orna",
    content: "CREATE SCHEMA std.;CREATE SCHEMA ;CREATE SCHEMA std;",
    source_bundle_hash: [
        0x7e, 0x67, 0xc9, 0x9b, 0x30, 0x05, 0xb6, 0x4f, 0x0e, 0x4f, 0x6a, 0xb9, 0xe4, 0xde, 0x40,
        0x3b, 0xe3, 0xb9, 0xdb, 0xb9, 0x57, 0x59, 0xe6, 0x57, 0x6d, 0x8e, 0x3e, 0x7f, 0xfb, 0xa4,
        0x80, 0xd8,
    ],
    source_revision_hash: [
        0x80, 0x16, 0x8f, 0xbd, 0xf3, 0xba, 0xa8, 0x30, 0x37, 0xd7, 0x17, 0xfc, 0xa8, 0xfd, 0xc3,
        0x02, 0x34, 0x11, 0x18, 0x79, 0xe1, 0x33, 0x0a, 0x27, 0x98, 0x0f, 0x4a, 0xa7, 0x65, 0x6c,
        0x61, 0xea,
    ],
    standard_digest: [
        0x6d, 0x3f, 0xaa, 0x32, 0x82, 0x0e, 0xeb, 0x73, 0x77, 0xc5, 0xbd, 0xfa, 0x3e, 0x8d, 0x6c,
        0xaf, 0xdc, 0x95, 0xa6, 0x7c, 0xbd, 0xef, 0x5b, 0x02, 0x63, 0x1f, 0x29, 0x1d, 0x14, 0xcc,
        0x68, 0xae,
    ],
};

const ALTERNATE_STANDARD_CONTEXT: StandardContextFixture = StandardContextFixture {
    source_unit: [14; 16],
    source_bundle: [15; 16],
    source_revision: [16; 16],
    standard_revision: [17; 16],
    catalogue_revision: [18; 16],
    logical_path: "std/alternate.orna",
    content: "CREATE SCHEMA std.;CREATE SCHEMA ;CREATE SCHEMA std;\n",
    source_bundle_hash: [
        0x9c, 0xb1, 0x72, 0x54, 0x07, 0x7f, 0xdb, 0xae, 0x68, 0x2d, 0x7b, 0xd8, 0x52, 0x91, 0x3f,
        0x91, 0xe6, 0x07, 0x44, 0x16, 0x1f, 0xc9, 0xee, 0x32, 0x20, 0xc9, 0xef, 0xc9, 0x9b, 0x5d,
        0x19, 0x2d,
    ],
    source_revision_hash: [
        0x67, 0x6a, 0xdc, 0x25, 0xfc, 0xc1, 0xd6, 0x7a, 0x53, 0xfe, 0x5d, 0x84, 0x2e, 0xdc, 0x0f,
        0xe3, 0x04, 0x61, 0x33, 0x7d, 0x95, 0x5a, 0x4f, 0x04, 0x78, 0x84, 0xd7, 0xed, 0xd1, 0x71,
        0x19, 0xab,
    ],
    standard_digest: [
        0xa3, 0xce, 0x6d, 0x48, 0x15, 0x61, 0x63, 0x33, 0x7b, 0xad, 0xe0, 0xae, 0xb9, 0x18, 0x6e,
        0x05, 0x00, 0x66, 0x20, 0x31, 0xe9, 0x0c, 0xae, 0x60, 0x14, 0x87, 0x1a, 0x7c, 0x3f, 0xd3,
        0xe1, 0x5a,
    ],
};

fn verified_standard_context(
    fixture: StandardContextFixture,
) -> orna_core::revision::VerifiedStandardLibrarySnapshot {
    let unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes(fixture.source_unit),
        0,
        fixture.logical_path,
        fixture.content,
        source_unit_content_digest(fixture.content).unwrap(),
    )
    .unwrap();
    let bundle = SourceBundleId::from_bytes(fixture.source_bundle);
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
    let source = StoredSourceRevision::new(
        bundle,
        SourceRevisionId::from_bytes(fixture.source_revision),
        None,
        vec![unit],
        bundle_hash,
        source_revision_record_digest(bundle, None, bundle_hash).unwrap(),
    )
    .unwrap();
    let snapshot = StandardLibrarySnapshot::new(
        StandardLibraryRevisionId::from_bytes(fixture.standard_revision),
        StandardLibraryDigestVersion::Version1,
        source,
        "orna.language/1",
        CatalogueSnapshot::new_with_types(
            CatalogueRevisionId::from_bytes(fixture.catalogue_revision),
            vec![],
            vec![],
            vec![],
            vec![],
        )
        .unwrap(),
        vec![],
        Sha256Digest::from_bytes(fixture.standard_digest),
    )
    .unwrap();

    verify_standard_library_snapshot(snapshot).unwrap()
}

fn assert_standard_context_identity(
    identity: StandardContextIdentity,
    fixture: StandardContextFixture,
) {
    assert_eq!(
        identity.standard_library_revision(),
        StandardLibraryRevisionId::from_bytes(fixture.standard_revision)
    );
    assert_eq!(
        identity.standard_catalogue_revision(),
        CatalogueRevisionId::from_bytes(fixture.catalogue_revision)
    );
    assert_eq!(
        identity.source_bundle(),
        SourceBundleId::from_bytes(fixture.source_bundle)
    );
    assert_eq!(
        identity.source_revision(),
        SourceRevisionId::from_bytes(fixture.source_revision)
    );
    assert_eq!(
        identity.source_bundle_hash(),
        Sha256Digest::from_bytes(fixture.source_bundle_hash)
    );
    assert_eq!(
        identity.source_revision_hash(),
        Sha256Digest::from_bytes(fixture.source_revision_hash)
    );
    assert_eq!(
        identity.standard_library_digest(),
        Sha256Digest::from_bytes(fixture.standard_digest)
    );
}

fn preflight_object_type() -> TypeId {
    TypeId::from_bytes([0x44; 16])
}

fn preflight_field() -> FieldId {
    FieldId::from_bytes([0x45; 16])
}

fn preflight_value_type() -> TypeId {
    orna_standard::BOOLEAN_TYPE_ID
}

fn preflight_active(
    standard: orna_core::revision::VerifiedStandardLibrarySnapshot,
) -> ActiveDatabaseRevision {
    let bundle = SourceBundleId::from_bytes([0x40; 16]);
    let bundle_hash = source_bundle_digest(&[]).unwrap();
    let source = StoredSourceRevision::new(
        bundle,
        SourceRevisionId::from_bytes([0x41; 16]),
        None,
        Vec::new(),
        bundle_hash,
        source_revision_record_digest(bundle, None, bundle_hash).unwrap(),
    )
    .unwrap();
    let catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x42; 16]),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let context = CatalogueHashContext::version_two(standard);
    let catalogue_hash =
        catalogue_digest_with_context(&context, &catalogue, &[], &[], &[], &[]).unwrap();
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(Vec::new(), Vec::new(), Vec::new(), Vec::new()),
        ),
        context,
    )
    .unwrap()
}

fn preflight_active_version_one() -> ActiveDatabaseRevision {
    let bundle = SourceBundleId::from_bytes([0x50; 16]);
    let bundle_hash = source_bundle_digest(&[]).unwrap();
    let source = StoredSourceRevision::new(
        bundle,
        SourceRevisionId::from_bytes([0x51; 16]),
        None,
        Vec::new(),
        bundle_hash,
        source_revision_record_digest(bundle, None, bundle_hash).unwrap(),
    )
    .unwrap();
    let catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x52; 16]),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let context = CatalogueHashContext::version_one();
    let catalogue_hash =
        catalogue_digest_with_context(&context, &catalogue, &[], &[], &[], &[]).unwrap();
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(source.id(), catalogue.revision()),
            source,
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(Vec::new(), Vec::new(), Vec::new(), Vec::new()),
        ),
        context,
    )
    .unwrap()
}

fn preflight_candidate(
    expected_base: RevisionPair,
    context: CatalogueHashContext,
    resolved_type: ResolvedType,
) -> DeployableRevision {
    let source_unit = SourceUnitId::from_bytes([0x46; 16]);
    let unit = StoredSourceUnit::new(
        source_unit,
        0,
        "preflight.orna",
        "",
        source_unit_content_digest("").unwrap(),
    )
    .unwrap();
    let bundle = SourceBundleId::from_bytes([0x47; 16]);
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
    let source = StoredSourceRevision::new(
        bundle,
        SourceRevisionId::from_bytes([0x48; 16]),
        Some(expected_base.source()),
        vec![unit],
        bundle_hash,
        source_revision_record_digest(bundle, Some(expected_base.source()), bundle_hash).unwrap(),
    )
    .unwrap();
    let schema = SchemaDefinition::new(
        SchemaId::from_bytes([0x49; 16]),
        QualifiedSemanticName::new(["preflight"]).unwrap(),
    );
    let object_type = ObjectTypeDefinition::new(
        preflight_object_type(),
        QualifiedSemanticName::new(["preflight", "flags"]).unwrap(),
        vec![FieldDefinition::new(
            preflight_field(),
            "enabled",
            0,
            resolved_type,
            false,
            true,
            None,
            None,
        )],
    );
    let catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x4a; 16]),
        vec![schema.clone()],
        vec![object_type.clone()],
    )
    .unwrap();
    let source_origin = SourceOrigin::new(source_unit, 0, 0).unwrap();
    let origins = vec![
        DefinitionOrigin::new(DefinitionIdentity::Schema(schema.id()), source_origin),
        DefinitionOrigin::new(
            DefinitionIdentity::ObjectType(object_type.id()),
            source_origin,
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Field {
                owner: object_type.id(),
                field: preflight_field(),
            },
            source_origin,
        ),
    ];
    let catalogue_hash =
        catalogue_digest_with_context(&context, &catalogue, &[], &[], &origins, &[]).unwrap();
    DeployableRevision::new_with_catalogue_hash_context(
        DeployableRevisionInput::new(
            expected_base,
            source,
            expected_base.catalogue(),
            catalogue,
            catalogue_hash,
            DeployableRevisionContent::new(origins, Vec::new(), Vec::new(), Vec::new())
                .with_current_function_revisions(Vec::new()),
        ),
        context,
    )
    .unwrap()
}

#[test]
fn candidate_preflight_accepts_a_version_two_value_before_physical_planning() {
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().unwrap(),
    )
    .unwrap();
    let active = preflight_active(standard.clone());
    let candidate = preflight_candidate(
        active.pair(),
        CatalogueHashContext::version_two(standard),
        ResolvedType::value(preflight_value_type()),
    );

    assert!(validate_candidate_preflight(&active, &candidate).is_ok());
    assert_eq!(
        plan_physical_changes(&active, &candidate),
        Err(PhysicalPlanError::UnsupportedUniqueField {
            object_type: preflight_object_type(),
            field: preflight_field(),
        })
    );
}

#[test]
fn candidate_encoder_projects_version_two_value_type_identity_and_pin() {
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().unwrap(),
    )
    .unwrap();
    let context = CatalogueHashContext::version_two(standard.clone());
    let catalogue =
        CatalogueSnapshot::new(CatalogueRevisionId::new(), Vec::new(), Vec::new()).unwrap();

    assert_eq!(
        CandidateEncoder::new(&context, &catalogue)
            .type_columns(ResolvedType::value(preflight_value_type()), false)
            .unwrap(),
        TypeColumns {
            kind: "value",
            scalar: None,
            target: None,
            value_type: Some(preflight_value_type()),
            standard_library_revision: Some(standard.revision()),
            enum_type: None,
            record_type: None,
        }
    );
}

#[test]
fn candidate_encoder_keeps_version_one_tuples_and_value_references_explicit() {
    let version_one = CatalogueHashContext::version_one();
    let catalogue =
        CatalogueSnapshot::new(CatalogueRevisionId::new(), Vec::new(), Vec::new()).unwrap();
    let version_one_encoder = CandidateEncoder::new(&version_one, &catalogue);
    assert_eq!(
        version_one_encoder
            .type_columns(ResolvedType::scalar(StandardScalar::Boolean), false)
            .unwrap(),
        TypeColumns {
            kind: "scalar",
            scalar: Some("boolean"),
            target: None,
            value_type: None,
            standard_library_revision: None,
            enum_type: None,
            record_type: None,
        }
    );
    assert_eq!(
        version_one_encoder
            .reference_target(DefinitionReferenceTarget::ObjectType(
                preflight_object_type(),
            ))
            .unwrap(),
        (
            "object_type",
            preflight_object_type().to_bytes().to_vec(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
    );

    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().unwrap(),
    )
    .unwrap();
    let version_two = CatalogueHashContext::version_two(standard.clone());
    let version_two_encoder = CandidateEncoder::new(&version_two, &catalogue);
    assert_eq!(
        version_two_encoder
            .reference_target(DefinitionReferenceTarget::ValueType(preflight_value_type()))
            .unwrap(),
        (
            "value_type",
            preflight_value_type().to_bytes().to_vec(),
            None,
            None,
            Some(standard.revision().to_bytes().to_vec()),
            None,
            None,
            None,
            None,
        )
    );
}

#[test]
fn candidate_encoder_separates_application_named_types() {
    let enum_type = TypeId::from_bytes([0x61; 16]);
    let record_type = TypeId::from_bytes([0x64; 16]);
    let catalogue = CatalogueSnapshot::new_with_record_value_types(
        CatalogueRevisionId::from_bytes([0x62; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x63; 16]),
            QualifiedSemanticName::new(["app"]).unwrap(),
        )],
        Vec::new(),
        Vec::new(),
        vec![EnumTypeDefinition::new(
            enum_type,
            QualifiedSemanticName::new(["app", "stage"]).unwrap(),
            ["lead", "customer"],
        )],
        vec![RecordValueTypeDefinition::new(
            record_type,
            QualifiedSemanticName::new(["app", "status"]).unwrap(),
            vec![
                RecordValueFieldDefinition::try_new_descriptor(
                    FieldId::from_bytes([0x65; 16]),
                    "stage",
                    0,
                    TypeDescriptor::named(enum_type),
                )
                .unwrap(),
            ],
        )],
        Vec::new(),
    )
    .unwrap();
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().unwrap(),
    )
    .unwrap();
    let context = CatalogueHashContext::version_two(standard);
    let encoder = CandidateEncoder::new(&context, &catalogue);

    assert_eq!(
        encoder
            .type_columns(ResolvedType::named(enum_type), false)
            .unwrap(),
        TypeColumns {
            kind: "enum",
            scalar: None,
            target: None,
            value_type: None,
            standard_library_revision: None,
            enum_type: Some(enum_type),
            record_type: None,
        }
    );
    assert_eq!(
        encoder
            .reference_target(DefinitionReferenceTarget::ValueType(enum_type))
            .unwrap(),
        (
            "enum_type",
            enum_type.to_bytes().to_vec(),
            None,
            None,
            None,
            Some(catalogue.revision().to_bytes().to_vec()),
            None,
            None,
            None,
        )
    );
    assert_eq!(
        encoder
            .type_columns(ResolvedType::named(record_type), false)
            .unwrap(),
        TypeColumns {
            kind: "record",
            scalar: None,
            target: None,
            value_type: None,
            standard_library_revision: None,
            enum_type: None,
            record_type: Some(record_type),
        }
    );
    assert_eq!(
        encoder
            .reference_target(DefinitionReferenceTarget::ValueType(record_type))
            .unwrap(),
        (
            "record_type",
            record_type.to_bytes().to_vec(),
            None,
            None,
            None,
            None,
            Some(catalogue.revision().to_bytes().to_vec()),
            None,
            None,
        )
    );
    let record_field = FieldId::from_bytes([0x65; 16]);
    assert_eq!(
        encoder
            .reference_target(DefinitionReferenceTarget::Field {
                owner: record_type,
                field: record_field,
            })
            .unwrap(),
        (
            "record_field",
            record_field.to_bytes().to_vec(),
            None,
            None,
            None,
            None,
            None,
            Some(catalogue.revision().to_bytes().to_vec()),
            Some(record_type.to_bytes().to_vec()),
        )
    );
}

#[test]
fn materialization_retains_candidate_context_for_context_aware_hashing() {
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().unwrap(),
    )
    .unwrap();
    let active = preflight_active(standard.clone());
    let candidate = preflight_candidate(
        active.pair(),
        CatalogueHashContext::version_two(standard),
        ResolvedType::value(preflight_value_type()),
    );

    let materialized = materialize(&candidate, &active).unwrap();
    assert_eq!(
        materialized.catalogue_hash_context.version(),
        candidate.catalogue_hash_context().version()
    );
    assert!(super::verify_candidate_hashes(&candidate, &materialized).is_ok());
}

#[test]
fn standard_upgrade_base_gate_has_no_normal_context_transition_check() {
    let active = preflight_active_version_one();
    let standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().unwrap(),
    )
    .unwrap();
    let candidate = preflight_candidate(
        active.pair(),
        CatalogueHashContext::version_two(standard),
        ResolvedType::value(preflight_value_type()),
    );

    assert!(validate_expected_base(&active, &candidate).is_ok());
    let stale = preflight_candidate(
        RevisionPair::new(
            SourceRevisionId::from_bytes([0x71; 16]),
            CatalogueRevisionId::from_bytes([0x72; 16]),
        ),
        candidate.catalogue_hash_context().clone(),
        ResolvedType::value(preflight_value_type()),
    );
    assert!(matches!(
        validate_expected_base(&active, &stale),
        Err(PostgresKernelError::ExpectedBaseMismatch { .. })
    ));
}

#[test]
fn reserved_identity_selector_keeps_active_before_inactive_raw_order() {
    let standard = orna_standard::StandardUpgradeIdentity::StandardLibraryRevision(
        StandardLibraryRevisionId::from_bytes([0x80; 16]),
    );
    let source =
        orna_standard::StandardUpgradeIdentity::SourceUnit(SourceUnitId::from_bytes([0x81; 16]));
    let inactive_earlier =
        orna_standard::StandardUpgradeIdentity::SourceUnit(SourceUnitId::from_bytes([0x82; 16]));
    let upgrade = vec![
        (
            source,
            SourceUnitId::from_bytes([0x81; 16]).to_bytes().to_vec(),
        ),
        (
            inactive_earlier,
            SourceUnitId::from_bytes([0x82; 16]).to_bytes().to_vec(),
        ),
    ];
    let active = vec![(
        standard,
        StandardLibraryRevisionId::from_bytes([0x80; 16])
            .to_bytes()
            .to_vec(),
    )];

    assert_eq!(first_active_reserved_identity(&active, &upgrade), None);
    let standard_upgrade = vec![(
        standard,
        StandardLibraryRevisionId::from_bytes([0x80; 16])
            .to_bytes()
            .to_vec(),
    )];
    assert_eq!(
        first_inactive_reserved_identity(
            &standard_upgrade,
            &[StandardLibraryRevisionId::from_bytes([0x80; 16])
                .to_bytes()
                .to_vec()],
        ),
        Some(standard)
    );
    assert_eq!(
        first_inactive_reserved_identity(
            &upgrade,
            &[
                SourceUnitId::from_bytes([0x82; 16]).to_bytes().to_vec(),
                SourceUnitId::from_bytes([0x81; 16]).to_bytes().to_vec(),
            ],
        ),
        Some(inactive_earlier)
    );
    let active_source = vec![(
        source,
        SourceUnitId::from_bytes([0x81; 16]).to_bytes().to_vec(),
    )];
    assert_eq!(
        first_active_reserved_identity(&active_source, &upgrade),
        Some(source)
    );
}

#[test]
fn candidate_preflight_preserves_expected_base_and_standard_context_precedence() {
    let active_standard = orna_standard::verify_standard_library_snapshot(
        orna_standard::retained_standard_library_snapshot().unwrap(),
    )
    .unwrap();
    let active = preflight_active(active_standard.clone());
    let matching_context = CatalogueHashContext::version_two(active_standard.clone());

    let stale_expected = RevisionPair::new(
        SourceRevisionId::from_bytes([0x50; 16]),
        CatalogueRevisionId::from_bytes([0x51; 16]),
    );
    let stale = preflight_candidate(
        stale_expected,
        matching_context.clone(),
        ResolvedType::value(preflight_value_type()),
    );
    assert!(matches!(
        validate_candidate_preflight(&active, &stale),
        Err(PostgresKernelError::ExpectedBaseMismatch {
            expected,
            active: actual_active,
        }) if expected == stale_expected && actual_active == active.pair()
    ));

    let version_one = preflight_candidate(
        active.pair(),
        CatalogueHashContext::version_one(),
        ResolvedType::scalar(StandardScalar::Boolean),
    );
    assert!(matches!(
        validate_candidate_preflight(&active, &version_one),
        Err(PostgresKernelError::StandardContextTransitionRequired {
            active: orna_core::revision::CatalogueHashVersion::Version2,
            candidate: orna_core::revision::CatalogueHashVersion::Version1,
        })
    ));

    let alternate_standard = verified_standard_context(ALTERNATE_STANDARD_CONTEXT);
    let different_context = preflight_candidate(
        active.pair(),
        CatalogueHashContext::version_two(alternate_standard.clone()),
        ResolvedType::named(preflight_object_type()),
    );
    let mismatch = validate_candidate_preflight(&active, &different_context).unwrap_err();
    let (actual_active, actual_candidate) = match mismatch {
        PostgresKernelError::StandardContextMismatch { active, candidate } => (active, candidate),
        other => {
            assert!(
                matches!(other, PostgresKernelError::StandardContextMismatch { .. }),
                "different verified version-two contexts must mismatch before persistence"
            );
            return;
        }
    };
    assert_eq!(
        *actual_active,
        StandardContextIdentity::from_verified_snapshot(&active_standard)
    );
    assert_eq!(
        *actual_candidate,
        StandardContextIdentity::from_verified_snapshot(&alternate_standard)
    );
}

#[test]
fn standard_context_guard_uses_core_verified_version_two_facts() {
    let active_standard = verified_standard_context(BASE_STANDARD_CONTEXT);
    let candidate_standard = verified_standard_context(ALTERNATE_STANDARD_CONTEXT);
    let active = StandardContextIdentity::from_verified_snapshot(&active_standard);
    let candidate = StandardContextIdentity::from_verified_snapshot(&candidate_standard);

    assert_standard_context_identity(active, BASE_STANDARD_CONTEXT);
    assert_standard_context_identity(candidate, ALTERNATE_STANDARD_CONTEXT);
    assert!(
        guard_standard_context_transition(
            &CatalogueHashContext::version_one(),
            &CatalogueHashContext::version_one(),
        )
        .is_ok()
    );

    let active_context = CatalogueHashContext::version_two(active_standard.clone());
    assert!(guard_standard_context_transition(&active_context, &active_context).is_ok());

    let transition =
        guard_standard_context_transition(&active_context, &CatalogueHashContext::version_one())
            .unwrap_err();
    assert!(matches!(
        transition,
        PostgresKernelError::StandardContextTransitionRequired {
            active: orna_core::revision::CatalogueHashVersion::Version2,
            candidate: orna_core::revision::CatalogueHashVersion::Version1,
        }
    ));

    let mismatch = guard_standard_context_transition(
        &active_context,
        &CatalogueHashContext::version_two(candidate_standard),
    )
    .unwrap_err();
    let (actual_active, actual_candidate) = match mismatch {
        PostgresKernelError::StandardContextMismatch { active, candidate } => (active, candidate),
        other => {
            assert!(
                matches!(other, PostgresKernelError::StandardContextMismatch { .. }),
                "version-two contexts must report a standard context mismatch"
            );
            return;
        }
    };
    assert_eq!(*actual_active, active);
    assert_eq!(*actual_candidate, candidate);
    let error = PostgresKernelError::StandardContextMismatch {
        active: actual_active,
        candidate: actual_candidate,
    };
    assert_eq!(
        error.to_string(),
        "the active and candidate standard contexts do not match"
    );
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn scalar_encoder_uses_the_complete_stable_postgres_vocabulary() {
    let expected = [
        (StandardScalar::Boolean, "boolean"),
        (StandardScalar::Integer, "integer"),
        (StandardScalar::BigInt, "bigint"),
        (StandardScalar::Float, "float"),
        (StandardScalar::Decimal, "decimal"),
        (
            StandardScalar::CharacterLargeObject,
            "character_large_object",
        ),
        (StandardScalar::BinaryLargeObject, "binary_large_object"),
        (StandardScalar::Uuid, "uuid"),
        (StandardScalar::Date, "date"),
        (StandardScalar::Time, "time"),
        (StandardScalar::Timestamp, "timestamp"),
        (StandardScalar::Duration, "duration"),
    ];
    for (value, spelling) in expected {
        assert_eq!(scalar(value, false).expect("storable scalar"), spelling);
    }
    assert!(scalar(StandardScalar::Void, false).is_err());
    assert_eq!(
        scalar(StandardScalar::Void, true).expect("single VOID"),
        "void"
    );
}

#[test]
fn type_encoder_preserves_closed_type_tuple_shapes() {
    let target = TypeId::from_bytes([3; 16]);
    assert_eq!(
        legacy_type_projection(ResolvedType::scalar(StandardScalar::Integer), false).unwrap(),
        LegacyTypeColumns::Scalar("integer")
    );
    assert_eq!(
        legacy_type_projection(ResolvedType::named(target), false).unwrap(),
        LegacyTypeColumns::Named(target)
    );
    assert_eq!(
        legacy_type_projection(ResolvedType::reference(target), false).unwrap(),
        LegacyTypeColumns::Reference(target)
    );
    assert_eq!(
        type_columns(ResolvedType::scalar(StandardScalar::Integer), false).unwrap(),
        ("scalar", Some("integer"), None)
    );
    assert_eq!(
        type_columns(ResolvedType::named(target), false).unwrap(),
        ("named", None, Some(target))
    );
    assert_eq!(
        type_columns(ResolvedType::reference(target), false).unwrap(),
        ("reference", None, Some(target))
    );
    assert!(type_columns(ResolvedType::scalar(StandardScalar::Void), false).is_err());
}

#[test]
fn standard_value_kind_encoder_preserves_opaque_definitions() {
    assert_eq!(
        standard_value_kind(ValueTypeKind::Primitive).unwrap(),
        "primitive"
    );
    assert_eq!(
        standard_value_kind(ValueTypeKind::Opaque).unwrap(),
        "opaque"
    );
}

#[test]
fn transaction_and_artifact_encoders_are_closed() {
    assert_eq!(function_transaction(None).unwrap(), None);
    assert_eq!(
        function_transaction(Some(FunctionTransaction::Atomic)).unwrap(),
        Some("atomic")
    );
    assert_eq!(
        function_transaction(Some(FunctionTransaction::ReadOnly)).unwrap(),
        Some("read_only")
    );
    assert!(function_transaction(Some(FunctionTransaction::Manual)).is_err());
    assert_eq!(artifact_kind(ExecutableArtifactKind::Server), "server_plan");
    assert_eq!(
        artifact_kind(ExecutableArtifactKind::Client),
        "client_bytecode"
    );
}

#[test]
fn reference_encoder_keeps_owner_qualified_targets() {
    let object = TypeId::from_bytes([1; 16]);
    let field = FieldId::from_bytes([2; 16]);
    let function = FunctionId::from_bytes([3; 16]);
    let parameter = ParameterId::from_bytes([4; 16]);
    let expression = ExpressionId::from_bytes([5; 16]);
    assert_eq!(
        reference_target(DefinitionReferenceTarget::ObjectType(object))
            .unwrap()
            .0,
        "object_type"
    );
    let field_target = reference_target(DefinitionReferenceTarget::Field {
        owner: object,
        field,
    })
    .unwrap();
    assert_eq!(field_target.0, "field");
    assert_eq!(field_target.2, Some(object.to_bytes().to_vec()));
    assert_eq!(
        reference_target(DefinitionReferenceTarget::Function(function))
            .unwrap()
            .0,
        "function"
    );
    let parameter_target = reference_target(DefinitionReferenceTarget::Parameter {
        owner: function,
        parameter,
    })
    .unwrap();
    assert_eq!(parameter_target.0, "parameter");
    assert_eq!(parameter_target.3, Some(function.to_bytes().to_vec()));
    assert_eq!(
        reference_target(DefinitionReferenceTarget::Expression(expression))
            .unwrap()
            .0,
        "expression"
    );
    let expected_kinds = [
        (DefinitionReferenceKind::FunctionCall, "function_call"),
        (DefinitionReferenceKind::NamedType, "named_type"),
        (DefinitionReferenceKind::ObjectReference, "object_reference"),
        (DefinitionReferenceKind::ParameterRead, "parameter_read"),
        (DefinitionReferenceKind::QueryObject, "query_object"),
        (DefinitionReferenceKind::QueryField, "query_field"),
        (DefinitionReferenceKind::Expression, "expression"),
        (DefinitionReferenceKind::WriteObject, "write_object"),
        (DefinitionReferenceKind::WriteField, "write_field"),
    ];
    assert_eq!(POSTGRES_REFERENCE_KINDS, expected_kinds.as_slice());
    assert_eq!(
        reference_kind(DefinitionReferenceKind::FunctionCall).unwrap(),
        "function_call"
    );
    assert_eq!(
        reference_kind(DefinitionReferenceKind::NamedType).unwrap(),
        "named_type"
    );
    assert_eq!(
        reference_kind(DefinitionReferenceKind::ObjectReference).unwrap(),
        "object_reference"
    );
    assert_eq!(
        reference_kind(DefinitionReferenceKind::ParameterRead).unwrap(),
        "parameter_read"
    );
    assert_eq!(
        reference_kind(DefinitionReferenceKind::QueryObject).unwrap(),
        "query_object"
    );
    assert_eq!(
        reference_kind(DefinitionReferenceKind::QueryField).unwrap(),
        "query_field"
    );
    assert_eq!(
        reference_kind(DefinitionReferenceKind::Expression).unwrap(),
        "expression"
    );
    assert_eq!(
        reference_kind(DefinitionReferenceKind::WriteObject).unwrap(),
        "write_object"
    );
    assert_eq!(
        reference_kind(DefinitionReferenceKind::WriteField).unwrap(),
        "write_field"
    );
}

#[test]
fn postgres_positive_integer_bounds_fail_closed() {
    assert_eq!(positive_i32(1, "test").unwrap(), 1);
    assert_eq!(positive_i32(i32::MAX as u32, "test").unwrap(), i32::MAX);
    assert!(positive_i32(0, "test").is_err());
    assert!(positive_i32(i32::MAX as u32 + 1, "test").is_err());
    assert_eq!(positive_i64(1, "test").unwrap(), 1);
    assert_eq!(positive_i64(i64::MAX as u64, "test").unwrap(), i64::MAX);
    assert!(positive_i64(0, "test").is_err());
    assert!(positive_i64(i64::MAX as u64 + 1, "test").is_err());
}

#[test]
fn standard_executable_contract_fails_closed_on_sequences_and_agreement() {
    let empty = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x30; 16]),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    assert!(
        validate_standard_executable_facts(StandardLibraryDigestVersion::Version1, &empty, &[],)
            .is_ok()
    );

    let executable = standard_executable_fixture();
    let function = executable_function_fixture(executable.revision().id());
    let second_function_id = FunctionId::from_bytes([0x20; 16]);
    let second_executable = standard_executable_fixture_with_function(
        second_function_id,
        FunctionRevisionId::from_bytes([0x20; 16]),
    );
    let second_function = executable_function_fixture_with_id(
        second_function_id,
        second_executable.revision().id(),
        QualifiedSemanticName::new(["std", "invoke", "other"]).unwrap(),
    );
    let catalogue = CatalogueSnapshot::new_with_functions(
        CatalogueRevisionId::from_bytes([0x30; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([1; 16]),
            QualifiedSemanticName::new(["std", "invoke"]).unwrap(),
        )],
        Vec::new(),
        vec![function.clone(), second_function],
    )
    .unwrap();

    assert!(
        validate_standard_executable_facts(
            StandardLibraryDigestVersion::Version1,
            &catalogue,
            std::slice::from_ref(&executable),
        )
        .is_err()
    );
    assert!(
            validate_standard_executable_facts(
                StandardLibraryDigestVersion::Version2,
                &catalogue,
                &[],
            )
            .is_err()
        );
    assert!(
        validate_standard_executable_facts(
            StandardLibraryDigestVersion::Version2,
            &catalogue,
            &[executable.clone(), executable.clone()],
        )
        .is_err()
    );
    assert!(
        validate_standard_executable_facts(
            StandardLibraryDigestVersion::Version2,
            &catalogue,
            std::slice::from_ref(&executable),
        )
        .is_err()
    );
    assert!(
        validate_standard_executable_facts(
            StandardLibraryDigestVersion::Version2,
            &catalogue,
            &[executable.clone(), second_executable.clone()],
        )
        .is_ok()
    );

    let wrong_revision =
        standard_executable_fixture_with_revision(FunctionRevisionId::from_bytes([0x55; 16]));
    let error = validate_standard_executable_facts(
        StandardLibraryDigestVersion::Version2,
        &catalogue,
        &[wrong_revision, second_executable],
    )
    .unwrap_err();
    assert!(matches!(
        error,
        PostgresKernelError::DurableInvariant { rule, .. }
            if rule == "standard catalogue function and executable current revision must agree"
    ));
}

#[test]
fn standard_resolved_type_columns_close_scalar_and_value_shapes() {
    let scalar =
        standard_resolved_type_columns(ResolvedType::scalar(StandardScalar::Integer), true)
            .unwrap();
    assert_eq!(scalar.kind, "scalar");
    assert_eq!(scalar.scalar, Some("integer"));
    assert!(scalar.value_type.is_none());

    let value_type = TypeId::from_bytes([0x77; 16]);
    let resolved = ResolvedType::value(value_type);
    let value = standard_resolved_type_columns(resolved, false).unwrap();
    assert_eq!(value.kind, "value");
    assert!(value.scalar.is_none());
    assert_eq!(value.value_type, Some(value_type));

    let void = standard_resolved_type_columns(ResolvedType::scalar(StandardScalar::Void), false);
    assert!(void.is_err());
    assert!(
        standard_resolved_type_columns(ResolvedType::scalar(StandardScalar::Void), true).is_ok()
    );
}

#[test]
fn standard_reference_target_columns_preserve_owner_and_pin_shapes() {
    let standard_revision = StandardLibraryRevisionId::from_bytes([0x44; 16]);
    let object = TypeId::from_bytes([1; 16]);
    let (kind, target, owner_type, owner_function, pin) = standard_reference_target_columns(
        DefinitionReferenceTarget::ObjectType(object),
        standard_revision,
    )
    .unwrap();
    assert_eq!(kind, "object_type");
    assert_eq!(target, object.to_bytes().to_vec());
    assert!(owner_type.is_none());
    assert!(owner_function.is_none());
    assert!(pin.is_none());

    let field = FieldId::from_bytes([2; 16]);
    let (kind, target, owner_type, owner_function, pin) = standard_reference_target_columns(
        DefinitionReferenceTarget::Field {
            owner: object,
            field,
        },
        standard_revision,
    )
    .unwrap();
    assert_eq!(kind, "field");
    assert_eq!(target, field.to_bytes().to_vec());
    assert_eq!(owner_type, Some(object.to_bytes().to_vec()));
    assert!(owner_function.is_none());
    assert!(pin.is_none());

    let function = FunctionId::from_bytes([0x10; 16]);
    let (kind, target, owner_type, owner_function, pin) = standard_reference_target_columns(
        DefinitionReferenceTarget::Function(function),
        standard_revision,
    )
    .unwrap();
    assert_eq!(kind, "function");
    assert_eq!(target, function.to_bytes().to_vec());
    assert!(owner_type.is_none());
    assert!(owner_function.is_none());
    assert!(pin.is_none());

    let parameter = ParameterId::from_bytes([0x10; 16]);
    let (kind, target, owner_type, owner_function, pin) = standard_reference_target_columns(
        DefinitionReferenceTarget::Parameter {
            owner: function,
            parameter,
        },
        standard_revision,
    )
    .unwrap();
    assert_eq!(kind, "parameter");
    assert_eq!(target, parameter.to_bytes().to_vec());
    assert!(owner_type.is_none());
    assert_eq!(owner_function, Some(function.to_bytes().to_vec()));
    assert!(pin.is_none());

    let value_type = TypeId::from_bytes([2; 16]);
    let (kind, target, owner_type, owner_function, pin) = standard_reference_target_columns(
        DefinitionReferenceTarget::ValueType(value_type),
        standard_revision,
    )
    .unwrap();
    assert_eq!(kind, "value_type");
    assert_eq!(target, value_type.to_bytes().to_vec());
    assert!(owner_type.is_none());
    assert!(owner_function.is_none());
    assert_eq!(pin, Some(standard_revision.to_bytes().to_vec()));
}

#[test]
fn standard_executable_identity_selectors_keep_active_before_inactive() {
    let active_function = (
        StandardExecutableIdentity::Function(FunctionId::from_bytes([1; 16])),
        vec![1; 16],
    );
    let inactive_function = (
        StandardExecutableIdentity::Function(FunctionId::from_bytes([2; 16])),
        vec![2; 16],
    );
    assert_eq!(
        first_active_standard_executable_identity(
            std::slice::from_ref(&active_function),
            &[inactive_function],
        ),
        None
    );
    assert_eq!(
        first_active_standard_executable_identity(
            std::slice::from_ref(&active_function),
            std::slice::from_ref(&active_function),
        ),
        Some(StandardExecutableIdentity::Function(
            FunctionId::from_bytes([1; 16])
        ))
    );
    let revision = (
        StandardExecutableIdentity::FunctionRevision(FunctionRevisionId::from_bytes([9; 16])),
        vec![9; 16],
    );
    assert_eq!(
        first_inactive_standard_executable_identity(
            std::slice::from_ref(&revision),
            &[vec![9; 16]]
        ),
        Some(StandardExecutableIdentity::FunctionRevision(
            FunctionRevisionId::from_bytes([9; 16])
        ))
    );
    assert_eq!(
        first_inactive_standard_executable_identity(&[revision], &[vec![8; 16]]),
        None
    );
}

#[test]
fn standard_parameter_selector_matches_scoped_pairs() {
    let function = FunctionId::from_bytes([0x10; 16]);
    let parameter = ParameterId::from_bytes([0x10; 16]);
    let wanted = StandardExecutableParameter {
        function,
        parameter,
    };
    assert_eq!(first_active_standard_parameter(&[], &[wanted]), None);
    assert_eq!(
        first_active_standard_parameter(&[wanted], &[wanted]),
        Some(wanted)
    );
    let other_owner = StandardExecutableParameter {
        function: FunctionId::from_bytes([0x11; 16]),
        parameter,
    };
    assert_eq!(
        first_active_standard_parameter(&[other_owner], &[wanted]),
        None
    );
}

fn standard_executable_fixture() -> StandardExecutable {
    standard_executable_fixture_with_function(
        FunctionId::from_bytes([0x10; 16]),
        FunctionRevisionId::from_bytes([0x10; 16]),
    )
}

fn standard_executable_fixture_with_revision(
    revision_id: FunctionRevisionId,
) -> StandardExecutable {
    standard_executable_fixture_with_function(FunctionId::from_bytes([0x10; 16]), revision_id)
}

fn standard_executable_fixture_with_function(
    function: FunctionId,
    revision_id: FunctionRevisionId,
) -> StandardExecutable {
    let artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        "orna.server-parameter-echo",
        1,
        vec![0x4f, 0x52, 0x4e, 0x41, 0x50, 0x45, 0, 0, 0, 0, 0, 1],
        Sha256Digest::from_bytes([7; 32]),
    )
    .unwrap();
    let revision = FunctionRevisionRecord::new(
        function,
        revision_id,
        1,
        SourceOrigin::new(SourceUnitId::from_bytes([3; 16]), 0, 1).unwrap(),
        Sha256Digest::from_bytes([5; 32]),
        Sha256Digest::from_bytes([6; 32]),
        "orna.language/1",
        artifact,
    )
    .unwrap();
    StandardExecutable::new(function, revision, Vec::new()).unwrap()
}

fn executable_function_fixture(current_revision: FunctionRevisionId) -> FunctionDefinition {
    executable_function_fixture_with_id(
        FunctionId::from_bytes([0x10; 16]),
        current_revision,
        QualifiedSemanticName::new(["std", "invoke", "echo"]).unwrap(),
    )
}

fn executable_function_fixture_with_id(
    function: FunctionId,
    current_revision: FunctionRevisionId,
    name: QualifiedSemanticName,
) -> FunctionDefinition {
    FunctionDefinition::new(
        function,
        name,
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer)),
        current_revision,
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    )
}
