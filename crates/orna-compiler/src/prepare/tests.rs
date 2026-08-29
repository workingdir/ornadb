use std::sync::atomic::{AtomicUsize, Ordering};

use orna_artifact::{
    client_plan::ActionTargetDomain,
    constant_expression::ConstantExpression,
    server_mutation_plan::{
        MutationExpressionKind as DurableMutationExpressionKind, ServerDeletePlan,
        ServerMutationOperation, ServerMutationPlan,
    },
    server_plan::{
        DistinctServerPlan, ExpressionKind, IdentitySelectedServerPlan, SelectBindValue,
        ServerPlan, ServerPlanError, UniqueTextSelectedServerPlan,
    },
};
use orna_core::{
    canonical_hash::{
        calculate_standard_library_digest, catalogue_digest, verify_standard_library_snapshot,
    },
    catalogue::{
        CatalogueSnapshot, FieldDefinition, FunctionDefinition, FunctionDomain, FunctionReturn,
        FunctionSecurity, FunctionTransaction, FunctionVolatility, ObjectTypeDefinition,
        ParameterDefinition, PreludeTypeName, SchemaDefinition, TypeBinding, ValueTypeDefinition,
        ValueTypeMutability, ValueTypePersistence,
    },
    revision::{
        ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
        CatalogueHashContext, DefinitionIdentity, DefinitionOrigin, DefinitionReferenceKind,
        DefinitionReferenceTarget, ExecutableArtifactKind, FunctionRevisionRecord, Sha256Digest,
        SourceOrigin, StandardLibraryDigestVersion, StandardLibrarySnapshot, StoredSourceRevision,
    },
    source::{SourceBundle, SourceUnit},
    types::{ResolvedType, StandardScalar},
};

use super::*;
use crate::{
    StandardApplicationCheckContext, check, check_standard_application,
    check_standard_library_source,
    mutation::{
        MutationAssignment, MutationExpression, MutationExpressionKind,
        MutationRecordFieldExpression, MutationRecordFieldExpressionKind, MutationValueType,
    },
};

#[test]
fn stream_signature_reference_sequence_includes_reference_element() {
    let target = TypeId::from_bytes([0xa1; 16]);
    let function = FunctionDefinition::new(
        FunctionId::from_bytes([0xa2; 16]),
        QualifiedSemanticName::new(["app", "events"]).unwrap(),
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Stream(ResolvedType::reference(target)),
        FunctionRevisionId::from_bytes([0xa3; 16]),
        FunctionSecurity::Invoker,
        None,
        FunctionVolatility::Stable,
    );

    assert_eq!(
        signature_reference_sequence(&function),
        vec![(
            DefinitionReferenceKind::ObjectReference,
            DefinitionReferenceTarget::ObjectType(target),
        )],
    );
}

#[test]
fn member_multiset_comparison_ignores_order_but_preserves_exact_multiplicity() {
    assert!(same_member_multiset(&[1_u8, 2, 2, 3], &[3, 2, 1, 2]));
    assert!(!same_member_multiset(&[1_u8, 2, 2, 3], &[3, 2, 1, 4]));
    assert!(!same_member_multiset(&[1_u8, 2, 2, 3], &[3, 2, 1, 1]));
}

#[test]
fn checked_value_type_reference_maps_to_its_durable_identity() {
    let checked = CheckedTypeId::Existing(TypeId::from_bytes([0x70; 16]));
    let durable = TypeId::from_bytes([0x71; 16]);
    let mut identities = IdentityMap::default();
    identities.types.insert(checked, durable);

    assert_eq!(
        identities
            .reference_target(CheckedDefinitionReferenceTarget::ValueType(checked))
            .unwrap(),
        DefinitionReferenceTarget::ValueType(durable)
    );
    assert!(matches!(
        identities.reference_target(CheckedDefinitionReferenceTarget::ValueType(
            CheckedTypeId::Existing(TypeId::from_bytes([0x72; 16]))
        )),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "checked type has no durable identity"
        })
    ));
}

#[test]
fn legacy_preparation_reaches_the_explicit_enum_hash_version_gate() {
    let active = empty_active();
    let report = checked_report(
        "CREATE SCHEMA crm; CREATE TYPE crm.stage AS ENUM ('lead', 'customer');",
        active.catalogue(),
    );
    assert!(report.diagnostics().is_empty());
    let checked = report.checked_bundle().unwrap();
    let mut allocations = CandidateAllocator::legacy();
    let identities = IdentityMap::build_legacy(checked, &active, &mut allocations).unwrap();
    let source = PreparedSource::new(
        report.parse_report(),
        active.pair().source(),
        &mut allocations,
    )
    .unwrap();
    let material = CandidateBuilder::new(
        report.parse_report(),
        checked,
        &active,
        identities,
        source,
        PreparationMode::LegacyV1,
        allocations.catalogue_revision(),
    )
    .materialise()
    .unwrap();
    let enum_type = &material.catalogue.enum_types()[0];
    assert_eq!(enum_type.name(), &semantic_name(&["crm", "stage"]));
    assert_eq!(enum_type.labels(), &["lead", "customer"]);
    assert!(
        material
            .origins
            .iter()
            .any(|origin| { origin.identity() == DefinitionIdentity::ValueType(enum_type.id()) })
    );

    assert!(matches!(
        prepare(&report, active.pair(), &active),
        Err(PrepareError::CanonicalHash(
            CanonicalHashError::CatalogueFactUnsupportedByHashVersion {
                fact: orna_core::canonical_hash::CatalogueHashFact::EnumTypeDefinition(_),
                ..
            }
        ))
    ));
}

const SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (name TEXT NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (\n\
            title TEXT DEFAULT 'todo',\n\
            completed BOOL NOT NULL DEFAULT FALSE,\n\
            priority INT DEFAULT 7,\n\
            note TEXT DEFAULT NULL,\n\
            assignee REF tasks.person ON DELETE SET NULL\n\
        );\n\
        CREATE SERVER FUNCTION tasks.open_tasks()\n\
        RETURNS ROWS (task REF tasks.task, title TEXT)\n\
        TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT REF(t), t.title FROM tasks.task t\n\
        WHERE t.completed = FALSE ORDER BY t.title;\n";

const REFORMATTED_SOURCE: &str = "-- source-only édit\n\
        CREATE SCHEMA tasks;\n\n\
        CREATE TYPE tasks.person AS OBJECT ( name TEXT NOT NULL );\n\
        CREATE TYPE tasks.task AS OBJECT (\n\
          title TEXT DEFAULT 'todo', completed BOOL NOT NULL DEFAULT FALSE,\n\
          priority INT DEFAULT 7, note TEXT DEFAULT NULL,\n\
          assignee REF tasks.person ON DELETE SET NULL\n\
        );\n\n\
        CREATE SERVER FUNCTION tasks.open_tasks()\n\
          RETURNS ROWS (task REF tasks.task, title TEXT)\n\
          TRANSACTION READ ONLY VOLATILITY STABLE\n\
          AS SELECT REF(t), t.title FROM tasks.task t\n\
          WHERE t.completed = FALSE ORDER BY t.title;\n";

const REQUIRED_UNIQUE_REFERENCE_SOURCE: &str = "CREATE SCHEMA relations;\n\
        CREATE TYPE relations.assignment AS OBJECT (\n\
            owner REF relations.owner NOT NULL UNIQUE\n\
        );\n\
        CREATE TYPE relations.owner AS OBJECT (name TEXT NOT NULL);\n";

const UNIQUE_TEXT_SOURCE: &str = "CREATE SCHEMA crm;\n\
        CREATE TYPE crm.contact AS OBJECT (\n\
            email TEXT UNIQUE,\n\
            name CHARACTER LARGE OBJECT NOT NULL UNIQUE\n\
        );\n";

static CATALOGUE_ALLOCATION: AtomicUsize = AtomicUsize::new(0);
static BUNDLE_ALLOCATION: AtomicUsize = AtomicUsize::new(0);
static REVISION_ALLOCATION: AtomicUsize = AtomicUsize::new(0);
static UNIT_ALLOCATION: AtomicUsize = AtomicUsize::new(0);
static SCHEMA_ALLOCATION: AtomicUsize = AtomicUsize::new(0);
static TYPE_ALLOCATION: AtomicUsize = AtomicUsize::new(0);

#[test]
fn mapped_candidate_type_selection_is_closed_and_retains_standard_identity() {
    let standard_id = TypeId::from_bytes([0x91; 16]);
    let reference_id = TypeId::from_bytes([0x92; 16]);
    let named_id = TypeId::from_bytes([0x93; 16]);

    assert_eq!(
        CandidateResolvedType::from_compatibility(ResolvedType::scalar(StandardScalar::Integer,))
            .unwrap(),
        CandidateResolvedType::LegacyScalar(StandardScalar::Integer)
    );
    assert_eq!(
        candidate_from_mapped_evidence(
            ResolvedType::scalar(StandardScalar::Boolean),
            Some(MappedEvidenceTarget::Value(standard_id)),
        )
        .unwrap(),
        CandidateResolvedType::StandardValue {
            type_id: standard_id,
            compatibility: StandardScalar::Boolean,
        }
    );
    assert_eq!(
        candidate_from_mapped_evidence(
            ResolvedType::named(named_id),
            Some(MappedEvidenceTarget::Named(named_id)),
        )
        .unwrap(),
        CandidateResolvedType::Named(named_id)
    );
    assert_eq!(
        candidate_from_mapped_evidence(
            ResolvedType::reference(reference_id),
            Some(MappedEvidenceTarget::ObjectReference(reference_id)),
        )
        .unwrap(),
        CandidateResolvedType::Reference(reference_id)
    );

    for (compatibility, expected) in [
        (
            ResolvedType::scalar(StandardScalar::Integer),
            CandidateResolvedType::LegacyScalar(StandardScalar::Integer),
        ),
        (
            ResolvedType::Named(TypeId::from_bytes([0x94; 16])),
            CandidateResolvedType::Named(TypeId::from_bytes([0x94; 16])),
        ),
        (
            ResolvedType::reference(TypeId::from_bytes([0x95; 16])),
            CandidateResolvedType::Reference(TypeId::from_bytes([0x95; 16])),
        ),
    ] {
        assert_eq!(
            candidate_from_mapped_evidence(compatibility, None).unwrap(),
            expected
        );
    }

    for (compatibility, evidence) in [
        (
            ResolvedType::Named(TypeId::from_bytes([0x96; 16])),
            MappedEvidenceTarget::Named(named_id),
        ),
        (
            ResolvedType::scalar(StandardScalar::Boolean),
            MappedEvidenceTarget::Named(named_id),
        ),
        (
            ResolvedType::Named(TypeId::from_bytes([0x96; 16])),
            MappedEvidenceTarget::Value(standard_id),
        ),
        (
            ResolvedType::Named(TypeId::from_bytes([0x96; 16])),
            MappedEvidenceTarget::ObjectReference(reference_id),
        ),
        (
            ResolvedType::reference(reference_id),
            MappedEvidenceTarget::Value(standard_id),
        ),
        (
            ResolvedType::scalar(StandardScalar::Boolean),
            MappedEvidenceTarget::ObjectReference(reference_id),
        ),
        (
            ResolvedType::reference(reference_id),
            MappedEvidenceTarget::ObjectReference(TypeId::from_bytes([0x97; 16])),
        ),
        (
            ResolvedType::scalar(StandardScalar::Boolean),
            MappedEvidenceTarget::Unknown,
        ),
        (
            ResolvedType::Named(TypeId::from_bytes([0x96; 16])),
            MappedEvidenceTarget::Unknown,
        ),
        (
            ResolvedType::reference(reference_id),
            MappedEvidenceTarget::Unknown,
        ),
    ] {
        let error = candidate_from_mapped_evidence(compatibility, Some(evidence)).unwrap_err();
        assert!(matches!(
            error,
            PrepareError::InvalidCheckedBundle {
                reason: "checked standard declaration type evidence disagrees with its semantic type",
            }
        ));
    }
}

#[test]
fn candidate_projection_policy_emits_value_identities_only_for_durable_v2_modes() {
    let type_id = TypeId::from_bytes([0x98; 16]);
    let candidate = CandidateResolvedType::StandardValue {
        type_id,
        compatibility: StandardScalar::Boolean,
    };
    let compatibility = ResolvedType::scalar(StandardScalar::Boolean);

    assert_ne!(
        CandidateTypeProjection::Compatibility,
        CandidateTypeProjection::Durable
    );
    for (mode, durable) in [
        (
            CandidateLoweringMode::LegacyV1,
            ResolvedType::scalar(StandardScalar::Boolean),
        ),
        (
            CandidateLoweringMode::StandardV1Match,
            ResolvedType::scalar(StandardScalar::Boolean),
        ),
        (
            CandidateLoweringMode::StandardV2Plan,
            ResolvedType::Value(type_id),
        ),
        (
            CandidateLoweringMode::StandardV2,
            ResolvedType::Value(type_id),
        ),
    ] {
        assert_eq!(
            mode.lower(candidate, CandidateTypeProjection::Compatibility),
            compatibility
        );
        assert_eq!(
            mode.lower(candidate, CandidateTypeProjection::Durable),
            durable
        );
    }
}

#[test]
fn declaration_evidence_lookup_preserves_the_slot_for_final_consumption() {
    let kind = crate::CheckedTypeUseKind::Return {
        owner: CheckedFunctionId::Existing(FunctionId::from_bytes([0x92; 16])),
        ordinal: 0,
    };
    let evidence = EvidenceUse {
        kind,
        target: EvidenceTarget::Value(TypeId::from_bytes([0x93; 16])),
        location: SourceLocation::from_syntax(
            "prepared.orna",
            &orna_syntax::SourceSpan { start: 4, end: 9 },
        ),
    };
    let mut declarations = DeclarationEvidence {
        ordered: vec![evidence.clone()],
        remaining: vec![evidence.clone()],
        consumed: Vec::new(),
    };

    assert_eq!(declarations.lookup(kind).unwrap(), evidence);
    assert_eq!(declarations.remaining.len(), 1);
    assert_eq!(declarations.consume(kind).unwrap(), evidence);
    assert!(declarations.is_empty());
    assert_eq!(declarations.consumed, vec![evidence]);
}

fn allocation_byte(counter: &AtomicUsize) -> u8 {
    if counter.fetch_add(1, Ordering::SeqCst) == 0 {
        1
    } else {
        2
    }
}

fn next_catalogue_id() -> CatalogueRevisionId {
    CatalogueRevisionId::from_bytes([allocation_byte(&CATALOGUE_ALLOCATION); 16])
}

fn next_bundle_id() -> SourceBundleId {
    SourceBundleId::from_bytes([allocation_byte(&BUNDLE_ALLOCATION); 16])
}

fn next_revision_id() -> SourceRevisionId {
    SourceRevisionId::from_bytes([allocation_byte(&REVISION_ALLOCATION); 16])
}

fn next_unit_id() -> SourceUnitId {
    SourceUnitId::from_bytes([allocation_byte(&UNIT_ALLOCATION); 16])
}

fn next_schema_id() -> SchemaId {
    SchemaId::from_bytes([allocation_byte(&SCHEMA_ALLOCATION); 16])
}

fn next_type_id() -> TypeId {
    TypeId::from_bytes([allocation_byte(&TYPE_ALLOCATION); 16])
}

fn next_function_revision_id() -> FunctionRevisionId {
    FunctionRevisionId::new()
}

static INVOCATION_CARRIER_TYPE_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

fn invocation_carrier_then_safe_type_id() -> TypeId {
    let allocation = INVOCATION_CARRIER_TYPE_ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
    INVOCATION_CARRIERS
        .get(allocation)
        .map_or(TypeId::from_bytes([0xf3; 16]), |carrier| carrier.id())
}

fn carrier_reservation_candidate_source() -> CandidateIdSource {
    CandidateIdSource {
        catalogue_revision: || CatalogueRevisionId::from_bytes([0xa1; 16]),
        source_bundle: || SourceBundleId::from_bytes([0xa2; 16]),
        source_revision: || SourceRevisionId::from_bytes([0xa3; 16]),
        source_unit: || SourceUnitId::from_bytes([0xa4; 16]),
        schema: || SchemaId::from_bytes([0xa5; 16]),
        type_id: invocation_carrier_then_safe_type_id,
        function_revision: || FunctionRevisionId::from_bytes([0xa6; 16]),
    }
}

fn invocation_carrier_standard() -> VerifiedStandardLibrarySnapshot {
    const SOURCE: &str = "CREATE SCHEMA std;CREATE SCHEMA std.types;CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;";
    let unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]),
        0,
        "std/types.orna",
        SOURCE,
        source_unit_content_digest(SOURCE).unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([5; 16]),
        SourceRevisionId::from_bytes([6; 16]),
        None,
        vec![unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([5; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let boolean = ValueTypeDefinition::primitive(
        TypeId::from_bytes([3; 16]),
        semantic_name(&["std", "types", "boolean"]),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.boolean@1",
    );
    let qualified =
        TypeBinding::qualified(semantic_name(&["std", "boolean"]), boolean.id()).unwrap();
    let prelude =
        TypeBinding::prelude(PreludeTypeName::new(["boolean"]).unwrap(), boolean.id()).unwrap();
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([8; 16]),
        vec![
            SchemaDefinition::new(SchemaId::from_bytes([1; 16]), semantic_name(&["std"])),
            SchemaDefinition::new(
                SchemaId::from_bytes([2; 16]),
                semantic_name(&["std", "types"]),
            ),
        ],
        Vec::new(),
        vec![boolean.clone()],
        vec![qualified.clone(), prelude.clone()],
    )
    .unwrap();
    let origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(SchemaId::from_bytes([1; 16])),
            SourceOrigin::new(source.units()[0].id(), 0, 18).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(SchemaId::from_bytes([2; 16])),
            SourceOrigin::new(source.units()[0].id(), 18, 42).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::ValueType(boolean.id()),
            SourceOrigin::new(source.units()[0].id(), 42, 159).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::TypeBinding(qualified.id()),
            SourceOrigin::new(source.units()[0].id(), 159, 204).unwrap(),
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::TypeBinding(prelude.id()),
            SourceOrigin::new(source.units()[0].id(), 204, 250).unwrap(),
        ),
    ];
    verify_standard_library_snapshot(
        StandardLibrarySnapshot::new(
            StandardLibraryRevisionId::from_bytes([7; 16]),
            StandardLibraryDigestVersion::Version1,
            source,
            "orna.language/1",
            catalogue,
            origins,
            Sha256Digest::from_bytes([
                0x10, 0x61, 0xb8, 0x16, 0x88, 0x39, 0xaa, 0x50, 0x60, 0xbd, 0x4e, 0x5a, 0xef, 0x1e,
                0xc8, 0x68, 0x08, 0x22, 0x02, 0xb2, 0x96, 0x91, 0x42, 0x2a, 0xd9, 0x1a, 0x29, 0x64,
                0x9c, 0x72, 0x0e, 0x83,
            ]),
        )
        .unwrap(),
    )
    .unwrap()
}

fn resource_standard() -> VerifiedStandardLibrarySnapshot {
    const SOURCE: &str = r#"CREATE SCHEMA std;
CREATE SCHEMA std.types;
CREATE TYPE std.types.CHARACTER_LARGE_OBJECT AS VALUE PRIMITIVE
    KERNEL CONTRACT 'orna.kernel.value.character-large-object@1'
    IMMUTABLE
    PERSISTABLE;
EXPORT TYPE std.types.CHARACTER_LARGE_OBJECT AS std.CHARACTER_LARGE_OBJECT;
EXPORT TYPE std.CHARACTER_LARGE_OBJECT TO PRELUDE AS CHARACTER LARGE OBJECT;
EXPORT TYPE std.CHARACTER_LARGE_OBJECT TO PRELUDE AS TEXT;
"#;
    let unit_id = SourceUnitId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
    let text_id = TypeId::from_bytes([4; 16]);
    let text = ValueTypeDefinition::primitive(
        text_id,
        semantic_name(&["std", "types", "character_large_object"]),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.character-large-object@1",
    );
    let qualified_text =
        TypeBinding::qualified(semantic_name(&["std", "character_large_object"]), text_id).unwrap();
    let prelude_character_large_object = TypeBinding::prelude(
        PreludeTypeName::new(["character", "large", "object"]).unwrap(),
        text_id,
    )
    .unwrap();
    let prelude_text =
        TypeBinding::prelude(PreludeTypeName::new(["text"]).unwrap(), text_id).unwrap();
    let origin = |identity, statement: &str| {
        let start = SOURCE.find(statement).unwrap();
        DefinitionOrigin::new(
            identity,
            SourceOrigin::new(
                unit_id,
                u32::try_from(start).unwrap(),
                u32::try_from(start + statement.len()).unwrap(),
            )
            .unwrap(),
        )
    };
    let origins = vec![
        origin(
            DefinitionIdentity::Schema(SchemaId::from_bytes([1; 16])),
            "CREATE SCHEMA std;",
        ),
        origin(
            DefinitionIdentity::Schema(SchemaId::from_bytes([2; 16])),
            "CREATE SCHEMA std.types;",
        ),
        origin(
            DefinitionIdentity::ValueType(text_id),
            "CREATE TYPE std.types.CHARACTER_LARGE_OBJECT AS VALUE PRIMITIVE\n    KERNEL CONTRACT 'orna.kernel.value.character-large-object@1'\n    IMMUTABLE\n    PERSISTABLE;",
        ),
        origin(
            DefinitionIdentity::TypeBinding(qualified_text.id()),
            "EXPORT TYPE std.types.CHARACTER_LARGE_OBJECT AS std.CHARACTER_LARGE_OBJECT;",
        ),
        origin(
            DefinitionIdentity::TypeBinding(prelude_character_large_object.id()),
            "EXPORT TYPE std.CHARACTER_LARGE_OBJECT TO PRELUDE AS CHARACTER LARGE OBJECT;",
        ),
        origin(
            DefinitionIdentity::TypeBinding(prelude_text.id()),
            "EXPORT TYPE std.CHARACTER_LARGE_OBJECT TO PRELUDE AS TEXT;",
        ),
    ];
    let unit = StoredSourceUnit::new(
        unit_id,
        0,
        "std/types.orna",
        SOURCE,
        source_unit_content_digest(SOURCE).unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([5; 16]),
        SourceRevisionId::from_bytes([6; 16]),
        None,
        vec![unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([5; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([8; 16]),
        vec![
            SchemaDefinition::new(SchemaId::from_bytes([1; 16]), semantic_name(&["std"])),
            SchemaDefinition::new(
                SchemaId::from_bytes([2; 16]),
                semantic_name(&["std", "types"]),
            ),
        ],
        Vec::new(),
        vec![text],
        vec![qualified_text, prelude_character_large_object, prelude_text],
    )
    .unwrap();
    let provisional = StandardLibrarySnapshot::new(
        StandardLibraryRevisionId::from_bytes([7; 16]),
        StandardLibraryDigestVersion::Version1,
        source.clone(),
        "orna.language/1",
        catalogue.clone(),
        origins.clone(),
        Sha256Digest::from_bytes([0; 32]),
    )
    .unwrap();
    let digest = calculate_standard_library_digest(&provisional).unwrap();
    verify_standard_library_snapshot(
        StandardLibrarySnapshot::new(
            provisional.revision(),
            provisional.digest_version(),
            source,
            provisional.language_version(),
            catalogue,
            origins,
            digest,
        )
        .unwrap(),
    )
    .unwrap()
}

fn action_standard() -> VerifiedStandardLibrarySnapshot {
    let base = resource_standard();
    const ACTION_SCHEMA_STATEMENT: &str = "CREATE SCHEMA std.action;";
    const ACTION_TYPE_STATEMENT: &str = "CREATE TYPE std.action.Action AS VALUE OPAQUE KERNEL CONTRACT 'orna.std.value.action@1' IMMUTABLE TRANSIENT;";
    const ACTION_BINDING_STATEMENT: &str = "EXPORT TYPE std.action.Action AS std.Action;";
    let mut source_content = base.source().units()[0].content().to_owned();
    source_content.push_str(ACTION_SCHEMA_STATEMENT);
    source_content.push('\n');
    source_content.push_str(ACTION_TYPE_STATEMENT);
    source_content.push('\n');
    source_content.push_str(ACTION_BINDING_STATEMENT);
    source_content.push('\n');

    let unit_id = base.source().units()[0].id();
    let unit = StoredSourceUnit::new(
        unit_id,
        0,
        "std/types.orna",
        &source_content,
        source_unit_content_digest(&source_content).unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
    let source = StoredSourceRevision::new(
        base.source().bundle(),
        base.source().id(),
        None,
        vec![unit],
        bundle_hash,
        source_revision_record_digest(base.source().bundle(), None, bundle_hash).unwrap(),
    )
    .unwrap();

    let action_schema_id = SchemaId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9]);
    let action_type_id = TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 20]);
    let action_binding =
        TypeBinding::qualified(semantic_name(&["std", "action"]), action_type_id).unwrap();
    let action_type = ValueTypeDefinition::opaque(
        action_type_id,
        semantic_name(&["std", "action", "action"]),
        "orna.std.value.action@1",
    );
    let mut schemas = base.catalogue().schemas().to_vec();
    schemas.push(SchemaDefinition::new(
        action_schema_id,
        semantic_name(&["std", "action"]),
    ));
    let mut value_types = base.catalogue().value_types().to_vec();
    value_types.push(action_type);
    let mut type_bindings = base.catalogue().type_bindings().to_vec();
    type_bindings.push(action_binding.clone());
    let catalogue = CatalogueSnapshot::new_with_types(
        base.catalogue().revision(),
        schemas,
        base.catalogue().object_types().to_vec(),
        value_types,
        type_bindings,
    )
    .unwrap();
    let origin = |identity, statement: &str| {
        let start = source_content.find(statement).unwrap();
        DefinitionOrigin::new(
            identity,
            SourceOrigin::new(
                unit_id,
                u32::try_from(start).unwrap(),
                u32::try_from(start + statement.len()).unwrap(),
            )
            .unwrap(),
        )
    };
    let mut origins = base.origins().to_vec();
    origins.push(origin(
        DefinitionIdentity::Schema(action_schema_id),
        ACTION_SCHEMA_STATEMENT,
    ));
    origins.push(origin(
        DefinitionIdentity::ValueType(action_type_id),
        ACTION_TYPE_STATEMENT,
    ));
    origins.push(origin(
        DefinitionIdentity::TypeBinding(action_binding.id()),
        ACTION_BINDING_STATEMENT,
    ));
    let provisional = StandardLibrarySnapshot::new(
        base.revision(),
        base.digest_version(),
        source.clone(),
        base.language_version(),
        catalogue.clone(),
        origins.clone(),
        Sha256Digest::from_bytes([0; 32]),
    )
    .unwrap();
    let digest = calculate_standard_library_digest(&provisional).unwrap();
    verify_standard_library_snapshot(
        StandardLibrarySnapshot::new(
            base.revision(),
            base.digest_version(),
            source,
            base.language_version(),
            catalogue,
            origins,
            digest,
        )
        .unwrap(),
    )
    .unwrap()
}

fn empty_standard_application_active(
    standard: &VerifiedStandardLibrarySnapshot,
) -> ActiveDatabaseRevision {
    let unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0x21; 16]),
        0,
        "application.orna",
        "",
        source_unit_content_digest("").unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x22; 16]),
        SourceRevisionId::from_bytes([0x23; 16]),
        None,
        vec![unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([0x22; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0x24; 16]),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let context = CatalogueHashContext::version_two(standard.clone());
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

#[test]
fn application_and_standard_preparation_retry_all_invocation_carriers_before_candidates() {
    assert_eq!(
        INVOCATION_CARRIERS
            .iter()
            .map(|carrier| carrier.id())
            .collect::<Vec<_>>(),
        vec![
            orna_core::system::SYS_INVOKE_VALUE_TYPE_ID,
            orna_core::system::SYS_INVOKE_REQUEST_TYPE_ID,
            orna_core::system::SYS_INVOKE_EVENT_TYPE_ID,
        ]
    );

    let active = empty_active();
    let report = checked_report(
        "CREATE SCHEMA app; CREATE TYPE app.item AS OBJECT (done BOOLEAN);",
        active.catalogue(),
    );
    INVOCATION_CARRIER_TYPE_ALLOCATIONS.store(0, Ordering::SeqCst);
    let application = prepare_with_allocator(
        &report,
        active.pair(),
        &active,
        CandidateAllocator::legacy_with_source(carrier_reservation_candidate_source()),
    )
    .unwrap();
    assert_eq!(
        application.candidate().object_types()[0].id(),
        TypeId::from_bytes([0xf3; 16])
    );
    assert_eq!(
        INVOCATION_CARRIER_TYPE_ALLOCATIONS.load(Ordering::SeqCst),
        INVOCATION_CARRIERS.len() + 1
    );

    let verified = invocation_carrier_standard();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_standard_application_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let bundle = SourceBundle::new([SourceUnit::new(
        "application.orna",
        "CREATE SCHEMA app; CREATE TYPE app.item AS OBJECT (done BOOLEAN);",
    )])
    .unwrap();
    let report = check_standard_application(&bundle, &context);
    assert!(report.diagnostics().is_empty());
    INVOCATION_CARRIER_TYPE_ALLOCATIONS.store(0, Ordering::SeqCst);
    let standard = prepare_standard_application_with_allocator(
        &report,
        active.pair(),
        &active,
        CandidateAllocator::with_source(
            ReservedStandardIds::from_snapshot(&verified),
            carrier_reservation_candidate_source(),
        ),
    )
    .unwrap();
    assert_eq!(
        standard.candidate().object_types()[0].id(),
        TypeId::from_bytes([0xf3; 16])
    );
    assert_eq!(
        INVOCATION_CARRIER_TYPE_ALLOCATIONS.load(Ordering::SeqCst),
        INVOCATION_CARRIERS.len() + 1
    );

    let mut untouched = CandidateAllocator::legacy_with_source(CandidateIdSource {
        type_id: || TypeId::from_bytes([0xe1; 16]),
        ..carrier_reservation_candidate_source()
    });
    assert_eq!(untouched.type_id(), TypeId::from_bytes([0xe1; 16]));
}

fn allocated_standard_upgrade_plan_for_construction_test() -> AllocatedStandardUpgradePlan {
    let unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0x74; 16]),
        0,
        "application.orna",
        "",
        source_unit_content_digest("").unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
    AllocatedStandardUpgradePlan {
        source_template: StoredSourceRevision::new(
            SourceBundleId::from_bytes([0x75; 16]),
            SourceRevisionId::from_bytes([0x76; 16]),
            None,
            vec![unit],
            bundle_hash,
            source_revision_record_digest(
                SourceBundleId::from_bytes([0x75; 16]),
                None,
                bundle_hash,
            )
            .unwrap(),
        )
        .unwrap(),
        source_ids: PreparedSourceIds {
            bundle: SourceBundleId::from_bytes([0x77; 16]),
            revision: SourceRevisionId::from_bytes([0x78; 16]),
            units: vec![SourceUnitId::from_bytes([0x79; 16])],
        },
        catalogue_revision: CatalogueRevisionId::from_bytes([0x7a; 16]),
        schemas: Vec::new(),
        object_types: Vec::new(),
        expressions: Vec::new(),
        origin_templates: Vec::new(),
        functions: Vec::new(),
    }
}

#[test]
fn standard_allocator_retries_each_same_class_reserved_identity() {
    CATALOGUE_ALLOCATION.store(0, Ordering::SeqCst);
    BUNDLE_ALLOCATION.store(0, Ordering::SeqCst);
    REVISION_ALLOCATION.store(0, Ordering::SeqCst);
    UNIT_ALLOCATION.store(0, Ordering::SeqCst);
    SCHEMA_ALLOCATION.store(0, Ordering::SeqCst);
    TYPE_ALLOCATION.store(0, Ordering::SeqCst);
    let mut reserved = ReservedStandardIds::default();
    reserved
        .catalogues
        .insert(CatalogueRevisionId::from_bytes([1; 16]));
    reserved
        .source_bundles
        .insert(SourceBundleId::from_bytes([1; 16]));
    reserved
        .source_revisions
        .insert(SourceRevisionId::from_bytes([1; 16]));
    reserved
        .source_units
        .insert(SourceUnitId::from_bytes([1; 16]));
    reserved.schemas.insert(SchemaId::from_bytes([1; 16]));
    reserved.types.insert(TypeId::from_bytes([1; 16]));
    let source = CandidateIdSource {
        catalogue_revision: next_catalogue_id,
        source_bundle: next_bundle_id,
        source_revision: next_revision_id,
        source_unit: next_unit_id,
        schema: next_schema_id,
        type_id: next_type_id,
        function_revision: next_function_revision_id,
    };
    let mut allocator = CandidateAllocator::with_source(reserved, source);

    assert_eq!(
        allocator.catalogue_revision(),
        CatalogueRevisionId::from_bytes([2; 16])
    );
    assert_eq!(
        allocator.source_bundle(),
        SourceBundleId::from_bytes([2; 16])
    );
    assert_eq!(
        allocator.source_revision(),
        SourceRevisionId::from_bytes([2; 16])
    );
    assert_eq!(allocator.source_unit(), SourceUnitId::from_bytes([2; 16]));
    assert_eq!(allocator.schema(), SchemaId::from_bytes([2; 16]));
    assert_eq!(allocator.type_id(), TypeId::from_bytes([2; 16]));
}

#[test]
fn standard_upgrade_final_construction_errors_preserve_their_exact_sources() {
    let catalogue = PrepareStandardUpgradeError::Catalogue {
        source: orna_core::catalogue::CatalogueSnapshotError::DuplicateSchemaId {
            id: SchemaId::from_bytes([0x71; 16]),
        },
    };
    assert!(matches!(
        &catalogue,
        PrepareStandardUpgradeError::Catalogue {
            source: orna_core::catalogue::CatalogueSnapshotError::DuplicateSchemaId { id },
        } if *id == SchemaId::from_bytes([0x71; 16])
    ));
    assert_eq!(
        catalogue.to_string(),
        format!(
            "the standard upgrade catalogue is invalid: {}",
            std::error::Error::source(&catalogue).unwrap()
        )
    );
    assert!(std::error::Error::source(&catalogue).is_some());

    let candidate_records = PrepareStandardUpgradeError::CandidateRecords {
        source: RevisionInvariantError::EmptyArtifactFormat,
    };
    assert_eq!(
        candidate_records.to_string(),
        format!(
            "the standard upgrade candidate records are invalid: {}",
            std::error::Error::source(&candidate_records).unwrap()
        )
    );
    assert!(std::error::Error::source(&candidate_records).is_some());

    let canonical = PrepareStandardUpgradeError::CanonicalHash {
        source: orna_core::canonical_hash::CanonicalHashError::LengthExceedsU32 {
            value: "upgrade test",
            length: usize::MAX,
        },
    };
    assert!(matches!(
        &canonical,
        PrepareStandardUpgradeError::CanonicalHash {
            source: orna_core::canonical_hash::CanonicalHashError::LengthExceedsU32 {
                value: "upgrade test",
                length,
            },
        } if *length == usize::MAX
    ));
    assert_eq!(
        canonical.to_string(),
        format!(
            "the standard upgrade canonical hashes are invalid: {}",
            std::error::Error::source(&canonical).unwrap()
        )
    );
    assert!(std::error::Error::source(&canonical).is_some());

    let revision = PrepareStandardUpgradeError::Revision {
        source: orna_core::revision::RevisionInvariantError::EmptyArtifactFormat,
    };
    assert!(matches!(
        revision,
        PrepareStandardUpgradeError::Revision {
            source: orna_core::revision::RevisionInvariantError::EmptyArtifactFormat,
        }
    ));
    assert_eq!(
        revision.to_string(),
        format!(
            "the standard upgrade revision is invalid: {}",
            std::error::Error::source(&revision).unwrap()
        )
    );
    assert!(std::error::Error::source(&revision).is_some());
}

#[test]
fn standard_upgrade_gate_eight_uses_the_real_catalogue_transition() {
    let mut plan = allocated_standard_upgrade_plan_for_construction_test();
    let duplicate = SchemaId::from_bytes([0x72; 16]);
    plan.schemas = vec![
        SchemaDefinition::new(duplicate, semantic_name(&["first"])),
        SchemaDefinition::new(duplicate, semantic_name(&["second"])),
    ];
    let catalogue_error = plan.into_catalogue().unwrap_err();

    assert!(matches!(
        catalogue_error,
        orna_core::catalogue::CatalogueSnapshotError::DuplicateSchemaId { id }
            if id == duplicate
    ));
}

#[test]
fn standard_upgrade_gate_nine_uses_the_real_candidate_record_transition() {
    let mut plan = allocated_standard_upgrade_plan_for_construction_test();
    plan.source_ids.revision = plan.source_template.id();
    let error = plan
        .into_catalogue()
        .unwrap()
        .into_candidate_records()
        .unwrap_err();

    assert!(matches!(
        error,
        RevisionInvariantError::SourceRevisionSelfParent { revision }
            if revision == SourceRevisionId::from_bytes([0x76; 16])
    ));
}

#[test]
fn standard_upgrade_gate_ten_uses_the_real_canonical_transition() {
    let mut records = allocated_standard_upgrade_plan_for_construction_test()
        .into_catalogue()
        .unwrap()
        .into_candidate_records()
        .unwrap();
    let unit = &records.source.units()[0];
    let invalid = StoredSourceUnit::new(
        unit.id(),
        unit.ordinal(),
        unit.logical_path(),
        unit.content(),
        Sha256Digest::from_bytes([0x7b; 32]),
    )
    .unwrap();
    records.source = StoredSourceRevision::new(
        records.source.bundle(),
        records.source.id(),
        records.source.parent(),
        vec![invalid],
        Sha256Digest::from_bytes([0; 32]),
        Sha256Digest::from_bytes([0; 32]),
    )
    .unwrap();
    let error = records
        .canonicalise(&CatalogueHashContext::version_one())
        .unwrap_err();

    assert!(matches!(
        error,
        CanonicalHashError::SourceContentHashMismatch { source_unit }
            if source_unit == SourceUnitId::from_bytes([0x79; 16])
    ));
}

#[test]
fn standard_upgrade_gate_eleven_follows_eight_nine_and_ten() {
    let candidate = allocated_standard_upgrade_plan_for_construction_test()
        .into_catalogue()
        .unwrap()
        .into_candidate_records()
        .unwrap()
        .canonicalise(&CatalogueHashContext::version_one())
        .unwrap();
    let active = empty_active();
    let error = candidate
        .into_deployable(&active, CatalogueHashContext::version_one())
        .unwrap_err();

    assert!(matches!(
        error,
        RevisionInvariantError::DeployableSourceParentMismatch { expected, actual }
            if expected == active.source().id()
                && actual == Some(SourceRevisionId::from_bytes([0x76; 16]))
    ));
}

#[test]
fn standard_upgrade_guard_admits_only_the_exact_append_only_source_child() {
    // The exact append-only child edge (work ADR 0059): the prepared
    // revision differs from the installed revision and its source parent
    // is the installed source revision.
    let v2_revision = StandardLibraryRevisionId::from_bytes([0x41; 16]);
    let v2_source = SourceRevisionId::from_bytes([0x42; 16]);
    let v3_revision = StandardLibraryRevisionId::from_bytes([0x43; 16]);
    assert!(admits_append_only_standard_child(
        v2_revision,
        v2_source,
        v3_revision,
        Some(v2_source),
    ));

    // A repeated install of the same revision is not a child.
    assert!(!admits_append_only_standard_child(
        v2_revision,
        v2_source,
        v2_revision,
        Some(v2_source),
    ));

    // A different revision whose source parent is a different source is
    // not the append-only child.
    assert!(!admits_append_only_standard_child(
        v2_revision,
        v2_source,
        v3_revision,
        Some(SourceRevisionId::from_bytes([0x44; 16])),
    ));

    // A different revision with no source parent is not a child.
    assert!(!admits_append_only_standard_child(
        v2_revision,
        v2_source,
        v3_revision,
        None,
    ));

    // An installed V3 pin with any prepared revision that is not the V3
    // child closes (the ADR wrong-base / already-installed matrix).
    let v3_source = SourceRevisionId::from_bytes([0x45; 16]);
    let other_revision = StandardLibraryRevisionId::from_bytes([0x46; 16]);
    assert!(!admits_append_only_standard_child(
        v3_revision,
        v3_source,
        v3_revision,
        None,
    ));
    assert!(!admits_append_only_standard_child(
        v3_revision,
        v3_source,
        other_revision,
        Some(v2_source),
    ));

    // The child edge is one-directional: the installed standard is not a
    // child of its own child.
    assert!(!admits_append_only_standard_child(
        v3_revision,
        SourceRevisionId::from_bytes([0x47; 16]),
        v2_revision,
        Some(v2_source),
    ));
}

#[test]
fn standard_upgrade_guard_rejects_the_same_installed_revision_with_the_exact_error() {
    let prepared_verified = invocation_carrier_standard();
    let prepared = check_standard_library_source(&prepared_verified).unwrap();

    // An installed pin of the same revision closes with the installed
    // revision before any further preparation runs.
    let installed_same = empty_standard_application_active(&prepared_verified);
    let error = prepare_checked_standard_upgrade_with_allocator(
        &prepared,
        &installed_same,
        CandidateAllocator::standard(&prepared_verified),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        PrepareStandardUpgradeError::StandardLibraryAlreadyInstalled { revision }
            if revision == prepared_verified.revision()
    ));
    assert_eq!(
        error.to_string(),
        format!(
            "standard library {} is already installed",
            prepared_verified.revision()
        )
    );
}

#[test]
fn accepts_all_supported_definition_reference_kinds() {
    let kinds = [
        DefinitionReferenceKind::FunctionCall,
        DefinitionReferenceKind::NamedType,
        DefinitionReferenceKind::ObjectReference,
        DefinitionReferenceKind::ParameterRead,
        DefinitionReferenceKind::QueryObject,
        DefinitionReferenceKind::QueryField,
        DefinitionReferenceKind::Expression,
        DefinitionReferenceKind::WriteObject,
        DefinitionReferenceKind::WriteField,
    ];

    assert_eq!(SUPPORTED_DEFINITION_REFERENCE_KINDS, kinds.as_slice());
    assert!(kinds.into_iter().all(supports_definition_reference_kind));
}

#[test]
fn active_field_rename_states_are_exact_and_fail_closed() {
    let owner = TypeId::from_bytes([9; 16]);
    let field_id = FieldId::from_bytes([10; 16]);
    let other_id = FieldId::from_bytes([11; 16]);
    let rename = CheckedFieldRename {
        owner: CheckedTypeId::Existing(owner),
        field: CheckedFieldId::Existing(field_id),
        old_name: "email".to_owned(),
        new_name: "primary_email".to_owned(),
    };
    let object =
        |fields| ObjectTypeDefinition::new(owner, semantic_name(&["people", "person"]), fields);
    let field = |id, name, ordinal| {
        FieldDefinition::new(
            id,
            name,
            ordinal,
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            false,
            false,
            None,
            None,
        )
    };
    assert!(
        validate_active_field_rename(&object(vec![field(field_id, "email", 0)]), &rename).is_ok()
    );
    assert!(
        validate_active_field_rename(&object(vec![field(field_id, "primary_email", 0)]), &rename)
            .is_ok()
    );
    assert!(matches!(
        validate_active_field_rename(
            &object(vec![
                field(field_id, "email", 0),
                field(other_id, "primary_email", 1)
            ]),
            &rename
        ),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "field rename active catalogue contains both names"
        })
    ));
    assert!(matches!(
        validate_active_field_rename(&object(vec![field(other_id, "email", 0)]), &rename),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "field rename names do not resolve to its checked field"
        })
    ));
    assert!(matches!(
        validate_active_field_rename(&object(vec![field(other_id, "other", 0)]), &rename),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "field rename active catalogue contains neither name"
        })
    ));
}

const CHANGED_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (name TEXT NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (\n\
            title TEXT DEFAULT 'todo',\n\
            completed BOOL NOT NULL DEFAULT FALSE,\n\
            priority INT DEFAULT 7,\n\
            note TEXT DEFAULT NULL,\n\
            assignee REF tasks.person ON DELETE SET NULL\n\
        );\n\
        CREATE SERVER FUNCTION tasks.open_tasks()\n\
        RETURNS ROWS (task REF tasks.task, title TEXT)\n\
        TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT REF(t), t.title FROM tasks.task t\n\
        WHERE t.completed = TRUE ORDER BY t.title;\n";

const SCALAR_SELECT_SOURCE: &str = "CREATE SCHEMA app;\n\
        CREATE TYPE app.item AS OBJECT (value INTEGER);\n\
        CREATE SERVER FUNCTION app.scalar()\n\
        RETURNS INTEGER\n\
        SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT i.value FROM app.item i;\n";

const DIRECT_BOOLEAN_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (active BOOL NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (owner REF tasks.person, completed BOOL NOT NULL);\n\
        CREATE SERVER FUNCTION tasks.active_tasks()\n\
        RETURNS ROWS (active BOOL, completed BOOL)\n\
        AS SELECT t.owner.active, t.completed FROM tasks.task t\n\
        WHERE t.owner.active ORDER BY t.completed DESC;\n";

const DIRECT_BOOLEAN_REFORMATTED_SOURCE: &str = "-- source-only direct-predicate edit\n\
        CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT ( active BOOL NOT NULL );\n\
        CREATE TYPE tasks.task AS OBJECT ( owner REF tasks.person, completed BOOL NOT NULL );\n\
        CREATE SERVER FUNCTION tasks.active_tasks()\n\
        RETURNS ROWS ( active BOOL, completed BOOL )\n\
        AS SELECT t.owner.active, t.completed\n\
        FROM tasks.task t WHERE t.owner.active ORDER BY t.completed DESC;\n";

const DIRECT_BOOLEAN_CHANGED_PREDICATE_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (active BOOL NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (owner REF tasks.person, completed BOOL NOT NULL);\n\
        CREATE SERVER FUNCTION tasks.active_tasks()\n\
        RETURNS ROWS (active BOOL, completed BOOL)\n\
        AS SELECT t.owner.active, t.completed FROM tasks.task t\n\
        WHERE t.completed ORDER BY t.completed DESC;\n";

const VERSION_ONE_REFERENCE_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (active BOOL NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (owner REF tasks.person);\n\
        CREATE SERVER FUNCTION tasks.owners()\n\
        RETURNS ROWS (owner REF tasks.person)\n\
        AS SELECT t.owner FROM tasks.task t;\n";

const IDENTITY_SELECTED_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL);\n\
        CREATE SERVER FUNCTION tasks.find(p_task REF tasks.task)\n\
        RETURNS ROWS (task REF tasks.task, title TEXT)\n\
        SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT REF(t), t.title FROM tasks.task t WHERE REF(t) = p_task;\n";

const UNIQUE_TEXT_SELECTED_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.task AS OBJECT (email TEXT UNIQUE, title TEXT NOT NULL);\n\
        CREATE SERVER FUNCTION tasks.by_email(p_email TEXT)\n\
        RETURNS ROWS (task REF tasks.task, title TEXT)\n\
        SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT REF(t), t.title FROM tasks.task t WHERE t.email = p_email;\n";

const IDENTITY_SELECTED_RENAMED_SELECTOR_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL);\n\
        CREATE SERVER FUNCTION tasks.find(selector REF tasks.task)\n\
        RETURNS ROWS (task REF tasks.task, title TEXT)\n\
        SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT REF(t), t.title FROM tasks.task t WHERE REF(t) = selector;\n";

const IDENTITY_SELECTED_NULLABLE_EQUALITY_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (name TEXT NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (owner REF tasks.person);\n\
        CREATE SERVER FUNCTION tasks.matches(p_task REF tasks.task)\n\
        RETURNS ROWS (name TEXT, same BOOL)\n\
        SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT t.owner.name, t.owner = t.owner FROM tasks.task t WHERE REF(t) = p_task;\n";

const DISTINCT_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (active BOOL NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (owner REF tasks.person, completed BOOL NOT NULL);\n\
        CREATE SERVER FUNCTION tasks.completion_values()\n\
        RETURNS ROWS (task REF tasks.task, active BOOL, completed BOOL)\n\
        SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT DISTINCT REF(t), t.owner.active, t.completed FROM tasks.task t\n\
        WHERE t.completed = TRUE;\n";

const DISTINCT_REFORMATTED_SOURCE: &str = "-- source-only DISTINCT edit\n\
        CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT ( active BOOL NOT NULL );\n\
        CREATE TYPE tasks.task AS OBJECT ( owner REF tasks.person, completed BOOL NOT NULL );\n\
        CREATE SERVER FUNCTION tasks.completion_values()\n\
        RETURNS ROWS ( task REF tasks.task, active BOOL, completed BOOL )\n\
        SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT DISTINCT REF(t), t.owner.active, t.completed\n\
        FROM tasks.task t WHERE t.completed = TRUE;\n";

const DISTINCT_REMOVED_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (active BOOL NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (owner REF tasks.person, completed BOOL NOT NULL);\n\
        CREATE SERVER FUNCTION tasks.completion_values()\n\
        RETURNS ROWS (task REF tasks.task, active BOOL, completed BOOL)\n\
        SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT REF(t), t.owner.active, t.completed FROM tasks.task t\n\
        WHERE t.completed = TRUE;\n";

const DISTINCT_REFERENCE_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (active BOOL NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (owner REF tasks.person);\n\
        CREATE SERVER FUNCTION tasks.owner_values()\n\
        RETURNS ROWS (owner REF tasks.person)\n\
        SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT DISTINCT t.owner FROM tasks.task t;\n";

const DIRECT_BOOLEAN_DISTINCT_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (active BOOL NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (owner REF tasks.person, completed BOOL NOT NULL);\n\
        CREATE SERVER FUNCTION tasks.visible_values()\n\
        RETURNS ROWS (completed BOOL)\n\
        SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT DISTINCT t.completed FROM tasks.task t WHERE t.owner.active;\n";

const DIRECT_BOOLEAN_DISTINCT_REFORMATTED_SOURCE: &str = "-- source-only direct DISTINCT edit\n\
        CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT ( active BOOL NOT NULL );\n\
        CREATE TYPE tasks.task AS OBJECT ( owner REF tasks.person, completed BOOL NOT NULL );\n\
        CREATE SERVER FUNCTION tasks.visible_values()\n\
        RETURNS ROWS ( completed BOOL )\n\
        SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT DISTINCT t.completed\n\
        FROM tasks.task AS t WHERE t.owner.active;\n";

const DIRECT_BOOLEAN_DISTINCT_CHANGED_PREDICATE_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (active BOOL NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (owner REF tasks.person, completed BOOL NOT NULL);\n\
        CREATE SERVER FUNCTION tasks.visible_values()\n\
        RETURNS ROWS (completed BOOL)\n\
        SECURITY INVOKER TRANSACTION READ ONLY VOLATILITY STABLE\n\
        AS SELECT DISTINCT t.completed FROM tasks.task t WHERE t.completed;\n";

const MUTATION_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (name TEXT NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL, done BOOL NOT NULL, note TEXT, owner REF tasks.person);\n\
        CREATE SERVER FUNCTION tasks.create(p_title TEXT, p_unused INT, p_owner REF tasks.person)\n\
        RETURNS ROWS (result REF tasks.task) TRANSACTION ATOMIC\n\
        AS INSERT INTO tasks.task AS created (title, done, note, owner)\n\
        VALUES (p_title, FALSE, NULL, p_owner) RETURNING REF(created);\n";

const MUTATION_REFORMATTED_SOURCE: &str = "-- source-only edit\n\
        CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT ( name TEXT NOT NULL );\n\
        CREATE TYPE tasks.task AS OBJECT ( title TEXT NOT NULL, done BOOL NOT NULL, note TEXT, owner REF tasks.person );\n\
        CREATE SERVER FUNCTION tasks.create( p_title TEXT, p_unused INT, p_owner REF tasks.person )\n\
        RETURNS ROWS ( result REF tasks.task ) TRANSACTION ATOMIC\n\
        AS INSERT INTO tasks.task AS created ( title, done, note, owner )\n\
        VALUES ( p_title, FALSE, NULL, p_owner ) RETURNING REF(created);\n";

const UPDATE_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (name TEXT NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL, done BOOL NOT NULL, owner REF tasks.person);\n\
        CREATE SERVER FUNCTION tasks.update(p_task REF tasks.task, p_title TEXT, p_owner REF tasks.person)\n\
        RETURNS ROWS (updated REF tasks.task) TRANSACTION ATOMIC\n\
        AS UPDATE tasks.task AS changed SET title = p_title, owner = p_owner\n\
        WHERE REF(changed) = p_task RETURNING REF(changed);\n";

const DELETE_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL);\n\
        CREATE SERVER FUNCTION tasks.remove(p_task REF tasks.task)\n\
        RETURNS ROWS (deleted BOOL) TRANSACTION ATOMIC\n\
        AS DELETE FROM tasks.task AS removed\n\
        WHERE REF(removed) = p_task RETURNING TRUE;\n";

const MUTATION_CHANGED_SOURCE: &str = "CREATE SCHEMA tasks;\n\
        CREATE TYPE tasks.person AS OBJECT (name TEXT NOT NULL);\n\
        CREATE TYPE tasks.task AS OBJECT (title TEXT NOT NULL, done BOOL NOT NULL, note TEXT, owner REF tasks.person);\n\
        CREATE SERVER FUNCTION tasks.create(p_title TEXT, p_unused INT, p_owner REF tasks.person)\n\
        RETURNS ROWS (result REF tasks.task) TRANSACTION ATOMIC\n\
        AS INSERT INTO tasks.task AS created (title, done, note, owner)\n\
        VALUES (p_title, TRUE, NULL, p_owner) RETURNING REF(created);\n";

const SHARED_EXPRESSION_SOURCE: &str = "CREATE SCHEMA demo;\n\
        CREATE TYPE demo.item AS OBJECT (first INT DEFAULT 1, second INT DEFAULT 1);\n";

#[test]
fn prepares_a_complete_source_catalogue_artifact_and_reference_revision() {
    let active = empty_active();
    let report = checked_report(SOURCE, active.catalogue());

    let prepared = prepare(&report, active.pair(), &active).unwrap();

    assert_eq!(prepared.expected_base(), active.pair());
    assert_eq!(prepared.source().parent(), Some(active.pair().source()));
    assert_eq!(prepared.source().units().len(), 1);
    assert_eq!(prepared.source().units()[0].logical_path(), "tasks.orna");
    assert_eq!(prepared.source().units()[0].content(), SOURCE);
    assert_eq!(
        source_unit_content_digest(SOURCE).unwrap(),
        prepared.source().units()[0].content_hash()
    );
    assert_eq!(
        source_bundle_digest(prepared.source().units()).unwrap(),
        prepared.source().bundle_hash()
    );
    assert_eq!(
        orna_core::canonical_hash::source_revision_digest(prepared.source()).unwrap(),
        prepared.source().revision_hash()
    );

    let catalogue = prepared.candidate();
    assert_eq!(catalogue.schemas().len(), 1);
    assert_eq!(catalogue.object_types().len(), 2);
    assert_eq!(catalogue.functions().len(), 1);
    assert_eq!(prepared.expressions().len(), 4);
    assert!(prepared.expressions().iter().all(|artifact| {
        artifact_payload_digest(artifact.payload()).unwrap() == artifact.content_hash()
    }));
    assert_eq!(prepared.new_function_revisions().len(), 1);
    assert_eq!(prepared.new_function_revisions()[0].revision_number(), 1);

    let task = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "task"]))
        .unwrap();
    let person = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "person"]))
        .unwrap();
    let title = task.field_by_name("title").unwrap();
    let completed = task.field_by_name("completed").unwrap();
    let priority = task.field_by_name("priority").unwrap();
    let note = task.field_by_name("note").unwrap();
    let assignee = task.field_by_name("assignee").unwrap();
    assert_eq!(
        assignee.resolved_type(),
        ResolvedType::reference(person.id())
    );
    assert_eq!(
        ConstantExpression::decode(expression(prepared.expressions(), title).payload()).unwrap(),
        ConstantExpression::Text("todo".to_owned())
    );
    assert_eq!(
        ConstantExpression::decode(expression(prepared.expressions(), completed).payload())
            .unwrap(),
        ConstantExpression::Boolean(false)
    );
    assert_eq!(
        ConstantExpression::decode(expression(prepared.expressions(), priority).payload()).unwrap(),
        ConstantExpression::Integer(7)
    );
    assert_eq!(
        ConstantExpression::decode(expression(prepared.expressions(), note).payload()).unwrap(),
        ConstantExpression::Null
    );

    let function = &catalogue.functions()[0];
    let revision = &prepared.new_function_revisions()[0];
    assert_eq!(function.current_revision(), revision.id());
    assert_eq!(
        artifact_payload_digest(revision.artifact().payload()).unwrap(),
        revision.artifact().content_hash()
    );
    let declaration_origin = revision.declaration_origin();
    let source = prepared
        .source()
        .units()
        .iter()
        .find(|unit| unit.id() == declaration_origin.source_unit())
        .unwrap();
    assert_eq!(
        function_declaration_digest(
            &source.content().as_bytes()
                [declaration_origin.byte_start() as usize..declaration_origin.byte_end() as usize]
        )
        .unwrap(),
        revision.declaration_content_hash()
    );
    let plan = ServerPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(revision.artifact().version(), SERVER_PLAN_VERSION);
    assert_eq!(
        IdentitySelectedServerPlan::decode(revision.artifact().payload()),
        Err(ServerPlanError::UnsupportedVersion(SERVER_PLAN_VERSION))
    );
    assert_eq!(
        DistinctServerPlan::decode(revision.artifact().payload()),
        Err(ServerPlanError::UnsupportedVersion(SERVER_PLAN_VERSION))
    );
    assert_eq!(plan.scan.object_type, task.id());
    assert!(matches!(
        plan.projections[0].kind,
        ExpressionKind::ObjectReference { .. }
    ));
    let ExpressionKind::FieldPath { ref steps, .. } = plan.projections[1].kind else {
        panic!("second projection is not a field path");
    };
    assert_eq!(steps[0].owner, task.id());
    assert_eq!(steps[0].field, title.id());

    assert_eq!(prepared.references().len(), 6);
    assert_eq!(
        prepared
            .references()
            .iter()
            .map(|reference| reference.ordinal())
            .collect::<Vec<_>>(),
        (0..6).collect::<Vec<_>>()
    );
    assert!(prepared.references().iter().all(|reference| {
        reference.source_function() == function.id() && reference.source_revision() == revision.id()
    }));
    assert_eq!(
        prepared
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::QueryObject,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: title.id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: completed.id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: title.id(),
                },
            ),
        ]
    );
    assert_eq!(prepared.origins().len(), 16);
    assert_eq!(
        catalogue_digest(
            catalogue,
            prepared.new_function_revisions(),
            prepared.expressions(),
            prepared.origins(),
            prepared.references(),
        )
        .unwrap(),
        prepared.catalogue_hash()
    );
}

#[test]
fn prepares_direct_boolean_predicates_as_version_one_server_plans_and_replays_by_semantics() {
    let empty = empty_active();
    let initial = prepare(
        &checked_report(DIRECT_BOOLEAN_SOURCE, empty.catalogue()),
        empty.pair(),
        &empty,
    )
    .unwrap();
    let catalogue = initial.candidate();
    let person = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "person"]))
        .unwrap();
    let task = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "task"]))
        .unwrap();
    let revision = &initial.new_function_revisions()[0];
    assert_eq!(revision.artifact().version(), SERVER_PLAN_VERSION);
    let plan = ServerPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(
        IdentitySelectedServerPlan::decode(revision.artifact().payload()),
        Err(ServerPlanError::UnsupportedVersion(SERVER_PLAN_VERSION))
    );
    assert_eq!(
        DistinctServerPlan::decode(revision.artifact().payload()),
        Err(ServerPlanError::UnsupportedVersion(SERVER_PLAN_VERSION))
    );
    let selection = plan
        .selection
        .as_ref()
        .expect("fixture has a direct predicate");
    let ExpressionKind::FieldPath { input, steps } = &selection.kind else {
        panic!("direct predicate must encode as a field path");
    };
    assert_eq!(*input, 0);
    assert_eq!(
        steps
            .iter()
            .map(|step| (step.owner, step.field))
            .collect::<Vec<_>>(),
        vec![
            (task.id(), task.field_by_name("owner").unwrap().id()),
            (person.id(), person.field_by_name("active").unwrap().id()),
        ]
    );
    assert_eq!(
        selection.value_type.resolved_type,
        ResolvedType::scalar(StandardScalar::Boolean)
    );
    assert!(selection.value_type.nullable);
    assert_eq!(
        initial
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::QueryObject,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("owner").unwrap().id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: person.id(),
                    field: person.field_by_name("active").unwrap().id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("completed").unwrap().id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("owner").unwrap().id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: person.id(),
                    field: person.field_by_name("active").unwrap().id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("completed").unwrap().id(),
                },
            ),
        ]
    );

    let initial_revision = revision.clone();
    let active = activate(&initial, vec![initial_revision.clone()], Vec::new());
    let replay = prepare(
        &checked_report(DIRECT_BOOLEAN_REFORMATTED_SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();
    assert!(replay.new_function_revisions().is_empty());
    assert_eq!(
        replay.candidate().functions()[0].current_revision(),
        initial_revision.id()
    );

    let changed = prepare(
        &checked_report(DIRECT_BOOLEAN_CHANGED_PREDICATE_SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();
    let changed_revision = &changed.new_function_revisions()[0];
    assert_ne!(changed_revision.id(), initial_revision.id());
    assert_ne!(
        changed_revision.semantic_hash(),
        initial_revision.semantic_hash()
    );
    assert_ne!(
        changed_revision.artifact().content_hash(),
        initial_revision.artifact().content_hash()
    );
}

#[test]
fn prepares_scalar_select_as_single_return_and_one_column_server_plan() {
    let active = empty_active();
    let report = checked_report(SCALAR_SELECT_SOURCE, active.catalogue());
    assert_eq!(report.diagnostics(), &[]);

    let prepared = prepare(&report, active.pair(), &active).unwrap();
    let function = &prepared.candidate().functions()[0];
    assert_eq!(
        function.return_type(),
        &FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Integer))
    );
    let revision = &prepared.new_function_revisions()[0];
    assert_eq!(revision.artifact().format(), SERVER_PLAN_FORMAT);
    assert_eq!(revision.artifact().version(), SERVER_PLAN_VERSION);
    let plan = ServerPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(plan.projections.len(), 1);
    assert_eq!(
        plan.projections[0].value_type.resolved_type,
        ResolvedType::scalar(StandardScalar::Integer)
    );
}

#[test]
fn prepares_identity_selected_query_as_a_version_two_server_plan() {
    let active = empty_active();
    let prepared = prepare(
        &checked_report(IDENTITY_SELECTED_SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();
    let catalogue = prepared.candidate();
    let task = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "task"]))
        .unwrap();
    let function = &catalogue.functions()[0];
    let revision = &prepared.new_function_revisions()[0];
    assert_eq!(revision.artifact().format(), SERVER_PLAN_FORMAT);
    assert_eq!(revision.artifact().version(), 2);
    assert_eq!(
        artifact_payload_digest(revision.artifact().payload()).unwrap(),
        revision.artifact().content_hash()
    );
    let plan = IdentitySelectedServerPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(plan.scan().object_type, task.id());
    assert_eq!(plan.selector().owner(), function.id());
    assert_eq!(plan.selector().parameter(), function.parameters()[0].id());
    assert!(ServerPlan::decode(revision.artifact().payload()).is_err());
    assert!(DistinctServerPlan::decode(revision.artifact().payload()).is_err());
    assert_eq!(
        prepared
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id())
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id())
            ),
            (
                DefinitionReferenceKind::QueryObject,
                DefinitionReferenceTarget::ObjectType(task.id())
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id())
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("title").unwrap().id()
                }
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id())
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter {
                    owner: function.id(),
                    parameter: function.parameters()[0].id()
                }
            ),
        ]
    );
}

#[test]
fn prepares_unique_text_selected_query_as_a_version_four_server_plan_with_exact_evidence() {
    let active = empty_active();
    let prepared = prepare(
        &checked_report(UNIQUE_TEXT_SELECTED_SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();
    let catalogue = prepared.candidate();
    let task = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "task"]))
        .unwrap();
    let function = &catalogue.functions()[0];
    let revision = &prepared.new_function_revisions()[0];
    let email = task.field_by_name("email").unwrap();
    assert_eq!(revision.artifact().format(), SERVER_PLAN_FORMAT);
    assert_eq!(revision.artifact().version(), 4);
    let plan = UniqueTextSelectedServerPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(plan.scan().object_type, task.id());
    assert_eq!(
        plan.selector(),
        &SelectBindValue::Text {
            scan_object_type: task.id(),
            field_owner: task.id(),
            field: email.id(),
            parameter_owner: function.id(),
            parameter: function.parameters()[0].id(),
            resolved_type: ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            field_nullable: true,
            parameter_required_non_null: true,
        }
    );
    assert!(ServerPlan::decode(revision.artifact().payload()).is_err());
    assert!(IdentitySelectedServerPlan::decode(revision.artifact().payload()).is_err());
    assert!(DistinctServerPlan::decode(revision.artifact().payload()).is_err());
    assert_eq!(
        prepared
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .filter(|(kind, _)| {
                matches!(
                    kind,
                    DefinitionReferenceKind::QueryObject
                        | DefinitionReferenceKind::QueryField
                        | DefinitionReferenceKind::ParameterRead
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::QueryObject,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("title").unwrap().id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: email.id(),
                },
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter {
                    owner: function.id(),
                    parameter: function.parameters()[0].id(),
                },
            ),
        ]
    );
}

#[test]
fn prepares_distinct_query_as_a_version_three_server_plan_with_exact_evidence() {
    let active = empty_active();
    let prepared = prepare(
        &checked_report(DISTINCT_SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();

    let catalogue = prepared.candidate();
    let person = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "person"]))
        .unwrap();
    let task = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "task"]))
        .unwrap();
    let function = &catalogue.functions()[0];
    let revision = &prepared.new_function_revisions()[0];
    assert_eq!(revision.artifact().kind(), ExecutableArtifactKind::Server);
    assert_eq!(revision.artifact().format(), SERVER_PLAN_FORMAT);
    assert_eq!(
        artifact_payload_digest(revision.artifact().payload()).unwrap(),
        revision.artifact().content_hash()
    );
    assert_eq!(revision.language_version(), SERVER_PLAN_LANGUAGE_VERSION);
    assert_eq!(
        function_semantic_digest(
            function,
            revision.language_version(),
            revision.artifact(),
            prepared.expressions(),
            prepared.references(),
        )
        .unwrap(),
        revision.semantic_hash()
    );
    assert_eq!(function.domain(), FunctionDomain::Server);
    assert_eq!(function.security(), FunctionSecurity::Invoker);
    assert_eq!(function.transaction(), Some(FunctionTransaction::ReadOnly));
    assert_eq!(function.volatility(), FunctionVolatility::Stable);
    assert!(function.parameters().is_empty());
    assert!(matches!(
        function.return_type(),
        FunctionReturn::Rows(columns)
            if columns.iter().map(FunctionReturnColumnDefinition::resolved_type).collect::<Vec<_>>()
                == vec![
                    ResolvedType::reference(task.id()),
                    ResolvedType::scalar(StandardScalar::Boolean),
                    ResolvedType::scalar(StandardScalar::Boolean),
                ]
    ));

    let plan = DistinctServerPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(revision.artifact().version(), plan.format_version());
    assert_eq!(plan.scan().input, 0);
    assert_eq!(plan.scan().object_type, task.id());
    assert_eq!(plan.projections().len(), 3);
    assert!(matches!(
        plan.projections()[0].kind,
        ExpressionKind::ObjectReference { input: 0 }
    ));
    assert_eq!(
        plan.projections()[0].value_type.resolved_type,
        ResolvedType::reference(task.id())
    );
    assert!(!plan.projections()[0].value_type.nullable);
    let ExpressionKind::FieldPath { input, steps } = &plan.projections()[1].kind else {
        panic!("second DISTINCT projection must be a field path");
    };
    assert_eq!(*input, 0);
    assert_eq!(
        steps
            .iter()
            .map(|step| (step.owner, step.field))
            .collect::<Vec<_>>(),
        vec![
            (task.id(), task.field_by_name("owner").unwrap().id()),
            (person.id(), person.field_by_name("active").unwrap().id()),
        ]
    );
    assert_eq!(
        plan.projections()[1].value_type.resolved_type,
        ResolvedType::scalar(StandardScalar::Boolean)
    );
    assert!(plan.projections()[1].value_type.nullable);
    let ExpressionKind::FieldPath { input, steps } = &plan.projections()[2].kind else {
        panic!("third DISTINCT projection must be a field path");
    };
    assert_eq!(*input, 0);
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].owner, task.id());
    assert_eq!(
        steps[0].field,
        task.field_by_name("completed").unwrap().id()
    );
    assert_eq!(
        plan.projections()[2].value_type.resolved_type,
        ResolvedType::scalar(StandardScalar::Boolean)
    );
    assert!(!plan.projections()[2].value_type.nullable);
    let selection = plan.selection().expect("fixture has a selection");
    assert_eq!(
        selection.value_type.resolved_type,
        ResolvedType::scalar(StandardScalar::Boolean)
    );
    assert!(!selection.value_type.nullable);
    assert!(matches!(selection.kind, ExpressionKind::Equality { .. }));
    assert!(ServerPlan::decode(revision.artifact().payload()).is_err());
    assert!(IdentitySelectedServerPlan::decode(revision.artifact().payload()).is_err());

    assert_eq!(
        prepared
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::QueryObject,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("owner").unwrap().id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: person.id(),
                    field: person.field_by_name("active").unwrap().id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("completed").unwrap().id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("completed").unwrap().id(),
                },
            ),
        ]
    );
    assert_eq!(
        prepared
            .references()
            .iter()
            .map(|reference| reference.ordinal())
            .collect::<Vec<_>>(),
        (0..7).collect::<Vec<_>>()
    );
}

#[test]
fn prepares_direct_boolean_distinct_predicates_as_v3_and_replays_by_semantics() {
    let empty = empty_active();
    let initial = prepare(
        &checked_report(DIRECT_BOOLEAN_DISTINCT_SOURCE, empty.catalogue()),
        empty.pair(),
        &empty,
    )
    .unwrap();
    let catalogue = initial.candidate();
    let person = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "person"]))
        .unwrap();
    let task = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "task"]))
        .unwrap();
    let function = &catalogue.functions()[0];
    let revision = &initial.new_function_revisions()[0];

    assert_eq!(revision.revision_number(), 1);
    assert_eq!(revision.artifact().kind(), ExecutableArtifactKind::Server);
    assert_eq!(revision.artifact().format(), SERVER_PLAN_FORMAT);
    assert_eq!(revision.language_version(), SERVER_PLAN_LANGUAGE_VERSION);
    assert_eq!(
        artifact_payload_digest(revision.artifact().payload()).unwrap(),
        revision.artifact().content_hash()
    );
    assert_eq!(
        function_semantic_digest(
            function,
            revision.language_version(),
            revision.artifact(),
            initial.expressions(),
            initial.references(),
        )
        .unwrap(),
        revision.semantic_hash()
    );

    let plan = DistinctServerPlan::decode(revision.artifact().payload()).unwrap();
    let format_version = plan.format_version();
    assert_eq!(revision.artifact().version(), format_version);
    assert_eq!(plan.encode().unwrap(), revision.artifact().payload());
    assert_eq!(
        ServerPlan::decode(revision.artifact().payload()),
        Err(ServerPlanError::UnsupportedVersion(format_version))
    );
    assert_eq!(
        IdentitySelectedServerPlan::decode(revision.artifact().payload()),
        Err(ServerPlanError::UnsupportedVersion(format_version))
    );
    assert_eq!(plan.scan().input, 0);
    assert_eq!(plan.scan().object_type, task.id());
    assert_eq!(plan.projections().len(), 1);
    let ExpressionKind::FieldPath { input, steps } = &plan.projections()[0].kind else {
        panic!("direct DISTINCT projection must encode as a field path");
    };
    assert_eq!(*input, 0);
    assert_eq!(
        steps
            .iter()
            .map(|step| (step.owner, step.field))
            .collect::<Vec<_>>(),
        vec![(task.id(), task.field_by_name("completed").unwrap().id())]
    );
    assert_eq!(
        plan.projections()[0].value_type.resolved_type,
        ResolvedType::scalar(StandardScalar::Boolean)
    );
    assert!(!plan.projections()[0].value_type.nullable);

    let selection = plan.selection().expect("fixture has a direct predicate");
    let ExpressionKind::FieldPath { input, steps } = &selection.kind else {
        panic!("direct DISTINCT predicate must encode as a field path");
    };
    assert_eq!(*input, 0);
    assert_eq!(
        steps
            .iter()
            .map(|step| (step.owner, step.field))
            .collect::<Vec<_>>(),
        vec![
            (task.id(), task.field_by_name("owner").unwrap().id()),
            (person.id(), person.field_by_name("active").unwrap().id()),
        ]
    );
    assert_eq!(
        selection.value_type.resolved_type,
        ResolvedType::scalar(StandardScalar::Boolean)
    );
    assert!(selection.value_type.nullable);

    assert_eq!(
        initial
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::QueryObject,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("completed").unwrap().id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("owner").unwrap().id(),
                },
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: person.id(),
                    field: person.field_by_name("active").unwrap().id(),
                },
            ),
        ]
    );
    assert_eq!(
        initial
            .references()
            .iter()
            .map(|reference| reference.ordinal())
            .collect::<Vec<_>>(),
        (0..4).collect::<Vec<_>>()
    );
    assert!(initial.references().iter().all(|reference| {
        reference.source_function() == function.id() && reference.source_revision() == revision.id()
    }));

    let initial_revision = revision.clone();
    let active = activate(&initial, vec![initial_revision.clone()], Vec::new());
    let replay = prepare(
        &checked_report(
            DIRECT_BOOLEAN_DISTINCT_REFORMATTED_SOURCE,
            active.catalogue(),
        ),
        active.pair(),
        &active,
    )
    .unwrap();
    assert!(replay.new_function_revisions().is_empty());
    assert_eq!(
        replay.candidate().functions()[0].current_revision(),
        initial_revision.id()
    );
    assert_ne!(replay.source().id(), active.source().id());
    assert_eq!(
        active.function_revisions(),
        std::slice::from_ref(&initial_revision)
    );
    assert_eq!(
        active.function_revisions()[0].artifact(),
        revision.artifact()
    );

    let changed = prepare(
        &checked_report(
            DIRECT_BOOLEAN_DISTINCT_CHANGED_PREDICATE_SOURCE,
            active.catalogue(),
        ),
        active.pair(),
        &active,
    )
    .unwrap();
    let changed_function = &changed.candidate().functions()[0];
    let changed_revision = &changed.new_function_revisions()[0];
    assert_eq!(changed_function.id(), function.id());
    assert_eq!(changed_revision.revision_number(), 2);
    assert_ne!(changed_revision.id(), initial_revision.id());
    assert_ne!(
        changed_revision.semantic_hash(),
        initial_revision.semantic_hash()
    );
    assert_ne!(
        changed_revision.artifact().content_hash(),
        initial_revision.artifact().content_hash()
    );
    assert_eq!(changed_revision.artifact().version(), format_version);
    let changed_plan = DistinctServerPlan::decode(changed_revision.artifact().payload()).unwrap();
    let changed_selection = changed_plan
        .selection()
        .expect("changed fixture has a direct predicate");
    let ExpressionKind::FieldPath { input, steps } = &changed_selection.kind else {
        panic!("changed direct DISTINCT predicate must encode as a field path");
    };
    assert_eq!(*input, 0);
    assert_eq!(
        steps
            .iter()
            .map(|step| (step.owner, step.field))
            .collect::<Vec<_>>(),
        vec![(task.id(), task.field_by_name("completed").unwrap().id())]
    );
    assert!(!changed_selection.value_type.nullable);

    let (mapped_plan, mapped_function, object_types, references) =
        mapped_distinct_fixture_for(DIRECT_BOOLEAN_DISTINCT_SOURCE);
    let mapped_person = object_types
        .iter()
        .find(|object_type| object_type.name() == &semantic_name(&["tasks", "person"]))
        .unwrap();
    let non_nullable_owner = object_types_with_task_field(
        &object_types,
        "owner",
        ResolvedType::reference(mapped_person.id()),
        false,
    );
    assert_preparation_reason(
        distinct_query_plan(
            &mapped_plan,
            &mapped_function,
            &non_nullable_owner,
            &references,
        ),
        "SELECT DISTINCT query field path type differs from its source field",
    );
}

#[test]
fn distinct_replay_reuses_its_revision_and_removing_distinct_creates_version_one() {
    let empty = empty_active();
    let initial = prepare(
        &checked_report(DISTINCT_SOURCE, empty.catalogue()),
        empty.pair(),
        &empty,
    )
    .unwrap();
    let initial_revision = initial.new_function_revisions()[0].clone();
    let active = activate(&initial, vec![initial_revision.clone()], Vec::new());

    for source in [DISTINCT_SOURCE, DISTINCT_REFORMATTED_SOURCE] {
        let replay = prepare(
            &checked_report(source, active.catalogue()),
            active.pair(),
            &active,
        )
        .unwrap();
        assert!(replay.new_function_revisions().is_empty());
        assert_eq!(
            replay.candidate().functions()[0].current_revision(),
            initial_revision.id()
        );
        assert_eq!(
            active.function_revisions(),
            std::slice::from_ref(&initial_revision)
        );
        assert_eq!(active.function_revisions()[0], initial_revision);
        assert_eq!(
            active.function_revisions()[0].artifact(),
            initial_revision.artifact()
        );
    }

    let removed = prepare(
        &checked_report(DISTINCT_REMOVED_SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();
    let changed = &removed.new_function_revisions()[0];
    assert_ne!(changed.id(), initial_revision.id());
    assert_ne!(changed.semantic_hash(), initial_revision.semantic_hash());
    assert_ne!(
        changed.artifact().content_hash(),
        initial_revision.artifact().content_hash()
    );
    assert_eq!(changed.artifact().version(), SERVER_PLAN_VERSION);
    assert!(ServerPlan::decode(changed.artifact().payload()).is_ok());
    assert!(DistinctServerPlan::decode(changed.artifact().payload()).is_err());
}

#[test]
fn distinct_preparation_validates_header_facts_in_the_accepted_order() {
    let (plan, function, object_types, references) = mapped_distinct_fixture();
    assert!(distinct_query_plan(&plan, &function, &object_types, &references).is_ok());

    assert_preparation_reason(
        distinct_query_plan(&plan, &function, &[], &references),
        "SELECT DISTINCT query scan object is absent from the candidate catalogue",
    );

    let function_with = |domain, parameters, return_type, security, transaction, volatility| {
        FunctionDefinition::new(
            function.id(),
            function.name().clone(),
            domain,
            parameters,
            return_type,
            function.current_revision(),
            security,
            transaction,
            volatility,
        )
    };
    let bad_mode = function_with(
        FunctionDomain::Client,
        function.parameters().to_vec(),
        function.return_type().clone(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_preparation_reason(
        distinct_query_plan(
            &plan,
            &bad_mode,
            &object_types,
            &distinct_query_reference_sequence(&plan, &bad_mode),
        ),
        "SELECT DISTINCT query function has unsupported execution modes",
    );

    let parameterised = function_with(
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            ParameterId::new(),
            "unexpected",
            0,
            ResolvedType::scalar(StandardScalar::Boolean),
            None,
        )],
        function.return_type().clone(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_preparation_reason(
        distinct_query_plan(
            &plan,
            &parameterised,
            &object_types,
            &distinct_query_reference_sequence(&plan, &parameterised),
        ),
        "SELECT DISTINCT query function declares parameters",
    );

    let single = function_with(
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_preparation_reason(
        distinct_query_plan(
            &plan,
            &single,
            &object_types,
            &distinct_query_reference_sequence(&plan, &single),
        ),
        "SELECT DISTINCT query function does not return ROWS",
    );

    let empty_rows = function_with(
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Rows(Vec::new()),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_preparation_reason(
        distinct_query_plan(
            &plan,
            &empty_rows,
            &object_types,
            &distinct_query_reference_sequence(&plan, &empty_rows),
        ),
        "SELECT DISTINCT query function returns empty ROWS",
    );

    let wrong_count = function_with(
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "one",
            0,
            ResolvedType::scalar(StandardScalar::Boolean),
        )]),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_preparation_reason(
        distinct_query_plan(
            &plan,
            &wrong_count,
            &object_types,
            &distinct_query_reference_sequence(&plan, &wrong_count),
        ),
        "SELECT DISTINCT query projection count differs from its function return",
    );
}

#[test]
fn version_one_preparation_revalidates_headers_facts_and_evidence_before_encoding() {
    let (plan, function, object_types, references) = mapped_version_one_fixture();
    assert!(version_one_query_plan(&plan, &function, &object_types, &references).is_ok());

    let function_with = |domain, parameters, return_type, security, transaction, volatility| {
        FunctionDefinition::new(
            function.id(),
            function.name().clone(),
            domain,
            parameters,
            return_type,
            function.current_revision(),
            security,
            transaction,
            volatility,
        )
    };
    for (transaction, volatility) in [
        (None, FunctionVolatility::Immutable),
        (
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        ),
        (
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Stable,
        ),
    ] {
        let accepted = function_with(
            FunctionDomain::Server,
            Vec::new(),
            function.return_type().clone(),
            FunctionSecurity::Invoker,
            transaction,
            volatility,
        );
        assert!(
            version_one_query_plan(
                &plan,
                &accepted,
                &object_types,
                &version_one_query_reference_sequence(&plan, &accepted),
            )
            .is_ok()
        );
    }

    assert_preparation_reason(
        version_one_query_plan(
            &plan.with_test_mutation(crate::relational::RelationalQueryTestMutation::InvalidScan),
            &function,
            &object_types,
            &references,
        ),
        "SERVER SELECT query scan object is absent from the candidate catalogue",
    );

    let manual = function_with(
        FunctionDomain::Server,
        Vec::new(),
        function.return_type().clone(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Manual),
        function.volatility(),
    );
    assert_preparation_reason(
        version_one_query_plan(
            &plan,
            &manual,
            &object_types,
            &version_one_query_reference_sequence(&plan, &manual),
        ),
        "SERVER SELECT query function has unsupported execution modes",
    );
    for (domain, security) in [
        (FunctionDomain::Client, FunctionSecurity::Invoker),
        (FunctionDomain::Server, FunctionSecurity::Definer),
    ] {
        let unsupported = function_with(
            domain,
            Vec::new(),
            function.return_type().clone(),
            security,
            function.transaction(),
            function.volatility(),
        );
        assert_preparation_reason(
            version_one_query_plan(
                &plan,
                &unsupported,
                &object_types,
                &version_one_query_reference_sequence(&plan, &unsupported),
            ),
            "SERVER SELECT query function has unsupported execution modes",
        );
    }

    let parameterised = function_with(
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            ParameterId::new(),
            "unexpected",
            0,
            ResolvedType::scalar(StandardScalar::Boolean),
            None,
        )],
        function.return_type().clone(),
        FunctionSecurity::Invoker,
        function.transaction(),
        function.volatility(),
    );
    assert_preparation_reason(
        version_one_query_plan(
            &plan,
            &parameterised,
            &object_types,
            &version_one_query_reference_sequence(&plan, &parameterised),
        ),
        "SERVER SELECT query function declares parameters",
    );

    let single = function_with(
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
        FunctionSecurity::Invoker,
        function.transaction(),
        function.volatility(),
    );
    assert_preparation_reason(
        version_one_query_plan(
            &plan,
            &single,
            &object_types,
            &version_one_query_reference_sequence(&plan, &single),
        ),
        "SERVER SELECT query function does not return ROWS",
    );

    let empty_rows = function_with(
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Rows(Vec::new()),
        FunctionSecurity::Invoker,
        function.transaction(),
        function.volatility(),
    );
    assert_preparation_reason(
        version_one_query_plan(
            &plan,
            &empty_rows,
            &object_types,
            &version_one_query_reference_sequence(&plan, &empty_rows),
        ),
        "SERVER SELECT query function returns empty ROWS",
    );

    let wrong_count = function_with(
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "only",
            0,
            ResolvedType::scalar(StandardScalar::Boolean),
        )]),
        FunctionSecurity::Invoker,
        function.transaction(),
        function.volatility(),
    );
    assert_preparation_reason(
        version_one_query_plan(
            &plan,
            &wrong_count,
            &object_types,
            &version_one_query_reference_sequence(&plan, &wrong_count),
        ),
        "SERVER SELECT query projection count differs from its function return",
    );

    let FunctionReturn::Rows(columns) = function.return_type() else {
        panic!("fixture must return rows");
    };
    let mut wrong_columns = columns.to_vec();
    wrong_columns[1] = FunctionReturnColumnDefinition::new(
        wrong_columns[1].name(),
        wrong_columns[1].ordinal(),
        ResolvedType::scalar(StandardScalar::Boolean),
    );
    let wrong_return = function_with(
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Rows(wrong_columns),
        FunctionSecurity::Invoker,
        function.transaction(),
        function.volatility(),
    );
    assert_preparation_reason(
        version_one_query_plan(
            &plan,
            &wrong_return,
            &object_types,
            &version_one_query_reference_sequence(&plan, &wrong_return),
        ),
        "SERVER SELECT query projection differs from its function return",
    );

    for (mutation, reason) in [
        (
            crate::relational::RelationalQueryTestMutation::InvalidProjectionFieldPathInput,
            "SERVER SELECT query field path has an invalid input or is empty",
        ),
        (
            crate::relational::RelationalQueryTestMutation::InvalidObjectReferenceInput,
            "SERVER SELECT query object reference has inconsistent facts",
        ),
        (
            crate::relational::RelationalQueryTestMutation::InvalidBooleanLiteralType,
            "SERVER SELECT query BOOLEAN expression has inconsistent type facts",
        ),
        (
            crate::relational::RelationalQueryTestMutation::InvalidEqualityType,
            "SERVER SELECT query equality expression has inconsistent type facts",
        ),
        (
            crate::relational::RelationalQueryTestMutation::InvalidOrderingFieldPathInput,
            "SERVER SELECT query field path has an invalid input or is empty",
        ),
        (
            crate::relational::RelationalQueryTestMutation::SelectionObjectReference,
            "SERVER SELECT query selection is not BOOLEAN",
        ),
    ] {
        let malformed = plan.with_test_mutation(mutation);
        assert_preparation_reason(
            version_one_query_plan(
                &malformed,
                &function,
                &object_types,
                &version_one_query_reference_sequence(&malformed, &function),
            ),
            reason,
        );
    }

    let unknown_field = plan
        .try_map_identities(Ok::<_, PrepareError>, |_| {
            Ok::<_, PrepareError>(FieldId::new())
        })
        .unwrap();
    assert_preparation_reason(
        version_one_query_plan(
            &unknown_field,
            &function,
            &object_types,
            &version_one_query_reference_sequence(&unknown_field, &function),
        ),
        "SERVER SELECT query field path field is absent from its source object",
    );
    let wrong_owner = plan
        .try_map_identities(
            {
                let mut calls = 0;
                move |type_id| {
                    calls += 1;
                    Ok::<_, PrepareError>(if calls == 3 { TypeId::new() } else { type_id })
                }
            },
            Ok::<_, PrepareError>,
        )
        .unwrap();
    assert_preparation_reason(
        version_one_query_plan(
            &wrong_owner,
            &function,
            &object_types,
            &version_one_query_reference_sequence(&wrong_owner, &function),
        ),
        "SERVER SELECT query field path owner differs from its source object",
    );
    assert_preparation_reason(
        version_one_query_plan(
            &plan,
            &function,
            &object_types_with_task_field(
                &object_types,
                "title",
                ResolvedType::scalar(StandardScalar::Boolean),
                true,
            ),
            &references,
        ),
        "SERVER SELECT query field path type differs from its source field",
    );
    assert_preparation_reason(
        version_one_query_plan(
            &plan,
            &function,
            &object_types_with_task_field(
                &object_types,
                "title",
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                false,
            ),
            &references,
        ),
        "SERVER SELECT query field path type differs from its source field",
    );
    assert_preparation_reason(
        plan.try_map_identities(
            |_| {
                Err::<TypeId, _>(PrepareError::InvalidCheckedBundle {
                    reason: "type mapping failure",
                })
            },
            Ok,
        ),
        "type mapping failure",
    );
    assert_preparation_reason(
        plan.try_map_identities(Ok::<_, PrepareError>, |_| {
            Err::<FieldId, _>(PrepareError::InvalidCheckedBundle {
                reason: "field mapping failure",
            })
        }),
        "field mapping failure",
    );

    let mut wrong_evidence = references.clone();
    wrong_evidence.reverse();
    assert_preparation_reason(
        version_one_query_plan(&plan, &function, &object_types, &wrong_evidence),
        "SERVER SELECT definition references differ from the checked function body",
    );
    assert_preparation_reason(
        version_one_query_plan(
            &plan,
            &function,
            &object_types,
            &references[..references.len() - 1],
        ),
        "SERVER SELECT definition references differ from the checked function body",
    );
    let mut extra_evidence = references.clone();
    extra_evidence.push(references[0]);
    assert_preparation_reason(
        version_one_query_plan(&plan, &function, &object_types, &extra_evidence),
        "SERVER SELECT definition references differ from the checked function body",
    );
    let mut wrong_kind = references.clone();
    wrong_kind[0].0 = DefinitionReferenceKind::QueryObject;
    assert_preparation_reason(
        version_one_query_plan(&plan, &function, &object_types, &wrong_kind),
        "SERVER SELECT definition references differ from the checked function body",
    );
    let mut wrong_target = references.clone();
    wrong_target[0].1 = DefinitionReferenceTarget::ObjectType(TypeId::new());
    assert_preparation_reason(
        version_one_query_plan(&plan, &function, &object_types, &wrong_target),
        "SERVER SELECT definition references differ from the checked function body",
    );

    let (direct_plan, direct_function, direct_objects, _) =
        mapped_version_one_fixture_for(DIRECT_BOOLEAN_SOURCE);
    let direct_references = version_one_query_reference_sequence(&direct_plan, &direct_function);
    assert_preparation_reason(
        version_one_query_plan(
            &direct_plan,
            &direct_function,
            &object_types_with_task_field(
                &direct_objects,
                "owner",
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                true,
            ),
            &direct_references,
        ),
        "SERVER SELECT query field path continues through a non-reference field",
    );
    let direct_task = direct_objects
        .iter()
        .find(|object_type| object_type.name() == &semantic_name(&["tasks", "task"]))
        .unwrap()
        .clone();
    assert_preparation_reason(
        version_one_query_plan(
            &direct_plan,
            &direct_function,
            std::slice::from_ref(&direct_task),
            &direct_references,
        ),
        "SERVER SELECT query field path target is absent from the candidate catalogue",
    );

    let (reference_plan, reference_function, reference_objects, _) =
        mapped_version_one_fixture_for(VERSION_ONE_REFERENCE_SOURCE);
    let reference_task = reference_objects
        .iter()
        .find(|object_type| object_type.name() == &semantic_name(&["tasks", "task"]))
        .unwrap()
        .clone();
    assert_preparation_reason(
        version_one_query_plan(
            &reference_plan,
            &reference_function,
            std::slice::from_ref(&reference_task),
            &version_one_query_reference_sequence(&reference_plan, &reference_function),
        ),
        "SERVER SELECT query field path target is absent from the candidate catalogue",
    );
}

#[test]
fn distinct_preparation_revalidates_candidate_facts_and_evidence() {
    let (plan, function, object_types, references) = mapped_distinct_fixture();
    let person = object_types
        .iter()
        .find(|object_type| object_type.name() == &semantic_name(&["tasks", "person"]))
        .unwrap()
        .clone();
    let task = object_types
        .iter()
        .find(|object_type| object_type.name() == &semantic_name(&["tasks", "task"]))
        .unwrap()
        .clone();
    let owner = task.field_by_name("owner").unwrap().clone();
    let completed = task.field_by_name("completed").unwrap().clone();

    let missing_field_plan = mapped_distinct_plan(&plan, Ok, |_| {
        Err::<FieldId, _>(PrepareError::InvalidCheckedBundle {
            reason: "mapping failure",
        })
    });
    assert_preparation_reason(missing_field_plan, "mapping failure");

    let missing_type_plan = mapped_distinct_plan(
        &plan,
        |_| {
            Err::<TypeId, _>(PrepareError::InvalidCheckedBundle {
                reason: "type mapping failure",
            })
        },
        Ok,
    );
    assert_preparation_reason(missing_type_plan, "type mapping failure");

    let object_reference_mismatch = mapped_distinct_plan(
        &plan,
        {
            let mut calls = 0;
            let task = task.id();
            let person = person.id();
            move |_| {
                calls += 1;
                Ok(if calls == 2 { person } else { task })
            }
        },
        Ok,
    )
    .unwrap();
    assert_preparation_reason(
        distinct_query_plan(
            &object_reference_mismatch,
            &function,
            &object_types,
            &distinct_query_reference_sequence(&object_reference_mismatch, &function),
        ),
        "SELECT DISTINCT query object reference has inconsistent facts",
    );

    let initial_owner_mismatch = mapped_distinct_plan(
        &plan,
        {
            let mut calls = 0;
            let task = task.id();
            let person = person.id();
            move |_| {
                calls += 1;
                Ok(if calls == 3 { person } else { task })
            }
        },
        Ok,
    )
    .unwrap();
    assert_preparation_reason(
        distinct_query_plan(
            &initial_owner_mismatch,
            &function,
            &object_types,
            &distinct_query_reference_sequence(&initial_owner_mismatch, &function),
        ),
        "SELECT DISTINCT query field path owner differs from its source object",
    );

    for (mutation, reason) in [
        (
            crate::relational::DistinctQueryTestMutation::InvalidFieldPathInput,
            "SELECT DISTINCT query field path has an invalid input or is empty",
        ),
        (
            crate::relational::DistinctQueryTestMutation::InvalidObjectReferenceInput,
            "SELECT DISTINCT query object reference has inconsistent facts",
        ),
        (
            crate::relational::DistinctQueryTestMutation::InvalidObjectReferenceType,
            "SELECT DISTINCT query object reference has inconsistent facts",
        ),
        (
            crate::relational::DistinctQueryTestMutation::InvalidBooleanLiteralType,
            "SELECT DISTINCT query BOOLEAN expression has inconsistent type facts",
        ),
        (
            crate::relational::DistinctQueryTestMutation::InvalidEqualityType,
            "SELECT DISTINCT query equality expression has inconsistent type facts",
        ),
    ] {
        let malformed = plan.with_test_mutation(mutation);
        assert_preparation_reason(
            distinct_query_plan(
                &malformed,
                &function,
                &object_types,
                &distinct_query_reference_sequence(&malformed, &function),
            ),
            reason,
        );
    }

    let unknown_field = mapped_distinct_plan(&plan, Ok, |field_id| {
        if field_id == owner.id() {
            Ok(FieldId::new())
        } else {
            Ok(field_id)
        }
    })
    .unwrap();
    assert_preparation_reason(
        distinct_query_plan(
            &unknown_field,
            &function,
            &object_types,
            &distinct_query_reference_sequence(&unknown_field, &function),
        ),
        "SELECT DISTINCT query field path field is absent from its source object",
    );

    let wrong_final_type = ObjectTypeDefinition::new(
        task.id(),
        task.name().clone(),
        vec![
            owner.clone(),
            FieldDefinition::new(
                completed.id(),
                completed.name(),
                completed.ordinal(),
                ResolvedType::scalar(StandardScalar::Integer),
                completed.nullable(),
                completed.unique(),
                completed.default_expression(),
                completed.on_delete(),
            ),
        ],
    );
    assert_preparation_reason(
        distinct_query_plan(
            &plan,
            &function,
            &[person.clone(), wrong_final_type],
            &references,
        ),
        "SELECT DISTINCT query field path type differs from its source field",
    );

    let nullable_final = ObjectTypeDefinition::new(
        task.id(),
        task.name().clone(),
        vec![
            owner.clone(),
            FieldDefinition::new(
                completed.id(),
                completed.name(),
                completed.ordinal(),
                completed.resolved_type(),
                true,
                completed.unique(),
                completed.default_expression(),
                completed.on_delete(),
            ),
        ],
    );
    assert_preparation_reason(
        distinct_query_plan(
            &plan,
            &function,
            &[person.clone(), nullable_final],
            &references,
        ),
        "SELECT DISTINCT query field path type differs from its source field",
    );

    let non_reference_owner = ObjectTypeDefinition::new(
        task.id(),
        task.name().clone(),
        vec![
            FieldDefinition::new(
                owner.id(),
                owner.name(),
                owner.ordinal(),
                ResolvedType::scalar(StandardScalar::Boolean),
                owner.nullable(),
                owner.unique(),
                owner.default_expression(),
                owner.on_delete(),
            ),
            completed.clone(),
        ],
    );
    assert_preparation_reason(
        distinct_query_plan(
            &plan,
            &function,
            &[person.clone(), non_reference_owner],
            &references,
        ),
        "SELECT DISTINCT query field path continues through a non-reference field",
    );

    let missing_target_owner = ObjectTypeDefinition::new(
        task.id(),
        task.name().clone(),
        vec![
            FieldDefinition::new(
                owner.id(),
                owner.name(),
                owner.ordinal(),
                ResolvedType::reference(TypeId::new()),
                owner.nullable(),
                owner.unique(),
                owner.default_expression(),
                owner.on_delete(),
            ),
            completed.clone(),
        ],
    );
    assert_preparation_reason(
        distinct_query_plan(
            &plan,
            &function,
            &[person.clone(), missing_target_owner],
            &references,
        ),
        "SELECT DISTINCT query field path target is absent from the candidate catalogue",
    );

    let wrong_return = FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        FunctionDomain::Server,
        Vec::new(),
        FunctionReturn::Rows(vec![
            FunctionReturnColumnDefinition::new("task", 0, ResolvedType::reference(task.id())),
            FunctionReturnColumnDefinition::new(
                "active",
                1,
                ResolvedType::scalar(StandardScalar::Boolean),
            ),
            FunctionReturnColumnDefinition::new(
                "completed",
                2,
                ResolvedType::scalar(StandardScalar::Integer),
            ),
        ]),
        function.current_revision(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    assert_preparation_reason(
        distinct_query_plan(
            &plan,
            &wrong_return,
            &object_types,
            &distinct_query_reference_sequence(&plan, &wrong_return),
        ),
        "SELECT DISTINCT query projection differs from its function return",
    );

    for invalid_references in [
        references[..references.len() - 1].to_vec(),
        {
            let mut extra = references.clone();
            extra.push(references[0]);
            extra
        },
        {
            let mut reordered = references.clone();
            reordered.reverse();
            reordered
        },
        {
            let mut wrong_kind = references.clone();
            wrong_kind[1].0 = DefinitionReferenceKind::QueryField;
            wrong_kind
        },
        {
            let mut wrong_target = references.clone();
            wrong_target[1].1 = DefinitionReferenceTarget::ObjectType(TypeId::new());
            wrong_target
        },
    ] {
        assert_preparation_reason(
            distinct_query_plan(&plan, &function, &object_types, &invalid_references),
            "SELECT DISTINCT definition references differ from the checked function body",
        );
    }
}

#[test]
fn distinct_preparation_has_an_exhaustive_projection_domain_and_boolean_selection() {
    let (plan, function, object_types, _) = mapped_distinct_fixture();
    let person = object_types
        .iter()
        .find(|object_type| object_type.name() == &semantic_name(&["tasks", "person"]))
        .unwrap();

    for scalar in StandardScalar::ALL {
        let semantic_type = SemanticType::scalar(scalar);
        let malformed = plan
            .with_test_mutation(crate::relational::DistinctQueryTestMutation::ClearSelection)
            .with_test_mutation(
                crate::relational::DistinctQueryTestMutation::ProjectionType {
                    semantic_type,
                    nullable: false,
                },
            );
        let candidate =
            object_types_with_distinct_completed_type(&object_types, ResolvedType::scalar(scalar));
        let function =
            distinct_function_with_completed_type(&function, ResolvedType::scalar(scalar));
        let references = distinct_query_reference_sequence(&malformed, &function);
        let accepted = matches!(
            scalar,
            StandardScalar::Boolean
                | StandardScalar::Integer
                | StandardScalar::BigInt
                | StandardScalar::BinaryLargeObject
        );
        let result = distinct_query_plan(&malformed, &function, &candidate, &references);
        if accepted {
            assert!(result.is_ok(), "{scalar:?} must be accepted: {result:?}");
        } else {
            assert_preparation_reason(
                result,
                "SELECT DISTINCT query projection has an unsupported type",
            );
        }
    }

    let reference = SemanticType::reference(person.id());
    let reference_plan = plan
        .with_test_mutation(crate::relational::DistinctQueryTestMutation::ClearSelection)
        .with_test_mutation(
            crate::relational::DistinctQueryTestMutation::ProjectionType {
                semantic_type: reference,
                nullable: false,
            },
        );
    let reference_function =
        distinct_function_with_completed_type(&function, ResolvedType::reference(person.id()));
    assert!(
        distinct_query_plan(
            &reference_plan,
            &reference_function,
            &object_types_with_distinct_completed_type(
                &object_types,
                ResolvedType::reference(person.id()),
            ),
            &distinct_query_reference_sequence(&reference_plan, &reference_function),
        )
        .is_ok()
    );

    let named_plan = plan
        .with_test_mutation(crate::relational::DistinctQueryTestMutation::ClearSelection)
        .with_test_mutation(
            crate::relational::DistinctQueryTestMutation::ProjectionType {
                semantic_type: SemanticType::Named(person.id()),
                nullable: false,
            },
        );
    let named_function =
        distinct_function_with_completed_type(&function, ResolvedType::Named(person.id()));
    assert_preparation_reason(
        distinct_query_plan(
            &named_plan,
            &named_function,
            &object_types_with_distinct_completed_type(
                &object_types,
                ResolvedType::Named(person.id()),
            ),
            &distinct_query_reference_sequence(&named_plan, &named_function),
        ),
        "SELECT DISTINCT query projection has an unsupported type",
    );

    let non_boolean_selection = plan
        .with_test_mutation(crate::relational::DistinctQueryTestMutation::SelectionObjectReference);
    assert_preparation_reason(
        distinct_query_plan(
            &non_boolean_selection,
            &function,
            &object_types,
            &distinct_query_reference_sequence(&non_boolean_selection, &function),
        ),
        "SELECT DISTINCT query selection is not BOOLEAN",
    );
}

#[test]
fn distinct_preparation_requires_the_final_projected_reference_target() {
    let (plan, function, object_types, references) =
        mapped_distinct_fixture_for(DISTINCT_REFERENCE_SOURCE);
    let task = object_types
        .iter()
        .find(|object_type| object_type.name() == &semantic_name(&["tasks", "task"]))
        .unwrap()
        .clone();

    assert_preparation_reason(
        distinct_query_plan(&plan, &function, &[task], &references),
        "SELECT DISTINCT query field path target is absent from the candidate catalogue",
    );
}

#[test]
fn identity_selected_query_replay_reuses_and_selector_rename_revises() {
    let empty = empty_active();
    let initial = prepare(
        &checked_report(IDENTITY_SELECTED_SOURCE, empty.catalogue()),
        empty.pair(),
        &empty,
    )
    .unwrap();
    let initial_revision = initial.new_function_revisions()[0].clone();
    let initial_parameter = initial.candidate().functions()[0].parameters()[0].id();
    let active = activate(&initial, vec![initial_revision.clone()], Vec::new());

    let replay = prepare(
        &checked_report(IDENTITY_SELECTED_SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();
    assert!(replay.new_function_revisions().is_empty());
    assert_eq!(
        replay.candidate().functions()[0].current_revision(),
        initial_revision.id()
    );
    assert_eq!(
        replay.candidate().functions()[0].parameters()[0].id(),
        initial_parameter
    );

    let renamed = prepare(
        &checked_report(
            IDENTITY_SELECTED_RENAMED_SELECTOR_SOURCE,
            active.catalogue(),
        ),
        active.pair(),
        &active,
    )
    .unwrap();
    let changed = &renamed.new_function_revisions()[0];
    assert_eq!(
        changed.revision_number(),
        initial_revision.revision_number() + 1
    );
    assert_ne!(changed.id(), initial_revision.id());
    assert_ne!(changed.semantic_hash(), initial_revision.semantic_hash());
    assert_ne!(
        changed.artifact().payload(),
        initial_revision.artifact().payload()
    );
    assert_ne!(
        renamed.candidate().functions()[0].parameters()[0].id(),
        initial_parameter
    );
    assert_eq!(changed.artifact().version(), 2);
}

#[test]
fn prepares_nullable_multi_hop_equality_projection_with_complete_evidence() {
    let active = empty_active();
    let prepared = prepare(
        &checked_report(
            IDENTITY_SELECTED_NULLABLE_EQUALITY_SOURCE,
            active.catalogue(),
        ),
        active.pair(),
        &active,
    )
    .unwrap();
    let task = prepared
        .candidate()
        .object_type_by_name(&semantic_name(&["tasks", "task"]))
        .unwrap();
    let person = prepared
        .candidate()
        .object_type_by_name(&semantic_name(&["tasks", "person"]))
        .unwrap();
    assert_eq!(
        prepared
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id())
            ),
            (
                DefinitionReferenceKind::QueryObject,
                DefinitionReferenceTarget::ObjectType(task.id())
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("owner").unwrap().id()
                }
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: person.id(),
                    field: person.field_by_name("name").unwrap().id()
                }
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("owner").unwrap().id()
                }
            ),
            (
                DefinitionReferenceKind::QueryField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.field_by_name("owner").unwrap().id()
                }
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id())
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter {
                    owner: prepared.candidate().functions()[0].id(),
                    parameter: prepared.candidate().functions()[0].parameters()[0].id()
                }
            ),
        ]
    );
}

#[test]
fn identity_selected_validator_rejects_private_plan_and_evidence_mismatches() {
    let active = empty_active();
    let prepared = prepare(
        &checked_report(IDENTITY_SELECTED_SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();
    let task = prepared.candidate().object_types()[0].clone();
    let function = prepared.candidate().functions()[0].clone();
    let checked = checked_report(IDENTITY_SELECTED_SOURCE, active.catalogue());
    let checked_function = &checked.checked_bundle().unwrap().server_functions()[0];
    let map = |owner, parameter, scan, field| {
        checked_function
            .identity_selected_query_plan()
            .unwrap()
            .try_map_identities(
                |_| Ok::<_, PrepareError>(scan),
                |_| Ok::<_, PrepareError>(field),
                |_| Ok::<_, PrepareError>(owner),
                |_| Ok::<_, PrepareError>(parameter),
            )
            .unwrap()
    };
    let plan = map(
        function.id(),
        function.parameters()[0].id(),
        task.id(),
        task.fields()[0].id(),
    );
    let references = identity_selected_query_reference_sequence(&plan, &function);
    let expect = |result: Result<_, PrepareError>, reason| {
        assert!(
            matches!(result, Err(PrepareError::InvalidCheckedBundle { reason: actual }) if actual == reason)
        );
    };
    expect(
        identity_selected_query_plan(
            &map(
                function.id(),
                function.parameters()[0].id(),
                TypeId::new(),
                task.fields()[0].id(),
            ),
            &function,
            std::slice::from_ref(&task),
            &references,
        ),
        "identity-selected query scan object is absent from the candidate catalogue",
    );
    let wrong_mode = FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        FunctionDomain::Server,
        function.parameters().to_vec(),
        function.return_type().clone(),
        function.current_revision(),
        FunctionSecurity::Definer,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    expect(
        identity_selected_query_plan(&plan, &wrong_mode, std::slice::from_ref(&task), &references),
        "identity-selected query function has unsupported execution modes",
    );
    let wrong_selector_type = FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            function.parameters()[0].id(),
            function.parameters()[0].name(),
            0,
            ResolvedType::reference(TypeId::new()),
            None,
        )],
        function.return_type().clone(),
        function.current_revision(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    expect(
        identity_selected_query_plan(
            &plan,
            &wrong_selector_type,
            std::slice::from_ref(&task),
            &references,
        ),
        "identity-selected query selector parameter does not reference its scan object",
    );
    let non_rows = FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        FunctionDomain::Server,
        function.parameters().to_vec(),
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
        function.current_revision(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    expect(
        identity_selected_query_plan(&plan, &non_rows, std::slice::from_ref(&task), &references),
        "identity-selected query function does not return ROWS",
    );
    let wrong_count = FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        FunctionDomain::Server,
        function.parameters().to_vec(),
        FunctionReturn::Rows(Vec::new()),
        function.current_revision(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    expect(
        identity_selected_query_plan(
            &plan,
            &wrong_count,
            std::slice::from_ref(&task),
            &references,
        ),
        "identity-selected query projection count differs from its function return",
    );
    let wrong_return_type = FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        FunctionDomain::Server,
        function.parameters().to_vec(),
        FunctionReturn::Rows(vec![
            FunctionReturnColumnDefinition::new("task", 0, ResolvedType::reference(task.id())),
            FunctionReturnColumnDefinition::new(
                "title",
                1,
                ResolvedType::scalar(StandardScalar::Boolean),
            ),
        ]),
        function.current_revision(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    expect(
        identity_selected_query_plan(
            &plan,
            &wrong_return_type,
            std::slice::from_ref(&task),
            &identity_selected_query_reference_sequence(&plan, &wrong_return_type),
        ),
        "identity-selected query projection differs from its function return",
    );
    let no_parameters = FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        FunctionDomain::Server,
        Vec::new(),
        function.return_type().clone(),
        function.current_revision(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    expect(
        identity_selected_query_plan(
            &plan,
            &no_parameters,
            std::slice::from_ref(&task),
            &references,
        ),
        "identity-selected query function does not declare exactly one parameter",
    );
    let two_parameters = FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        FunctionDomain::Server,
        vec![
            function.parameters()[0].clone(),
            function.parameters()[0].clone(),
        ],
        function.return_type().clone(),
        function.current_revision(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    expect(
        identity_selected_query_plan(
            &plan,
            &two_parameters,
            std::slice::from_ref(&task),
            &references,
        ),
        "identity-selected query function does not declare exactly one parameter",
    );
    let default_parameter = ParameterDefinition::new(
        function.parameters()[0].id(),
        function.parameters()[0].name(),
        0,
        function.parameters()[0].resolved_type(),
        Some(ExpressionId::new()),
    );
    let with_default = FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        FunctionDomain::Server,
        vec![default_parameter],
        function.return_type().clone(),
        function.current_revision(),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    expect(
        identity_selected_query_plan(
            &plan,
            &with_default,
            std::slice::from_ref(&task),
            &references,
        ),
        "identity-selected query selector parameter has an unsupported default expression",
    );
    expect(
        identity_selected_query_plan(
            &map(
                FunctionId::new(),
                function.parameters()[0].id(),
                task.id(),
                task.fields()[0].id(),
            ),
            &function,
            std::slice::from_ref(&task),
            &references,
        ),
        "identity-selected query selector owner differs from its enclosing function",
    );
    expect(
        identity_selected_query_plan(
            &map(
                function.id(),
                ParameterId::new(),
                task.id(),
                task.fields()[0].id(),
            ),
            &function,
            std::slice::from_ref(&task),
            &references,
        ),
        "identity-selected query selector parameter is not its enclosing function parameter",
    );
    expect(
        identity_selected_query_plan(
            &map(
                function.id(),
                function.parameters()[0].id(),
                task.id(),
                FieldId::new(),
            ),
            &function,
            std::slice::from_ref(&task),
            &references,
        ),
        "identity-selected query field path field is absent from its source object",
    );
    let wrong_final_type = ObjectTypeDefinition::new(
        task.id(),
        task.name().clone(),
        vec![FieldDefinition::new(
            task.fields()[0].id(),
            task.fields()[0].name(),
            0,
            ResolvedType::scalar(StandardScalar::Boolean),
            false,
            false,
            None,
            None,
        )],
    );
    expect(
        identity_selected_query_plan(
            &plan,
            &function,
            std::slice::from_ref(&wrong_final_type),
            &references,
        ),
        "identity-selected query field path type differs from its source field",
    );
    let nullable_final = ObjectTypeDefinition::new(
        task.id(),
        task.name().clone(),
        vec![FieldDefinition::new(
            task.fields()[0].id(),
            task.fields()[0].name(),
            0,
            task.fields()[0].resolved_type(),
            true,
            false,
            None,
            None,
        )],
    );
    expect(
        identity_selected_query_plan(
            &plan,
            &function,
            std::slice::from_ref(&nullable_final),
            &references,
        ),
        "identity-selected query field path type differs from its source field",
    );
    let mut wrong_evidence = references.clone();
    wrong_evidence.reverse();
    expect(
        identity_selected_query_plan(
            &plan,
            &function,
            std::slice::from_ref(&task),
            &wrong_evidence,
        ),
        "parameterised SELECT definition references differ from the checked function body",
    );
    expect(
        identity_selected_query_plan(
            &plan,
            &function,
            std::slice::from_ref(&task),
            &references[..references.len() - 1],
        ),
        "parameterised SELECT definition references differ from the checked function body",
    );
    let mut extra_evidence = references.clone();
    extra_evidence.push(references[0]);
    expect(
        identity_selected_query_plan(
            &plan,
            &function,
            std::slice::from_ref(&task),
            &extra_evidence,
        ),
        "parameterised SELECT definition references differ from the checked function body",
    );
    let mut wrong_target_evidence = references.clone();
    wrong_target_evidence[0].1 = DefinitionReferenceTarget::ObjectType(TypeId::new());
    expect(
        identity_selected_query_plan(
            &plan,
            &function,
            std::slice::from_ref(&task),
            &wrong_target_evidence,
        ),
        "parameterised SELECT definition references differ from the checked function body",
    );
    let checked_plan = checked_function.identity_selected_query_plan().unwrap();
    assert_eq!(
        checked_plan
            .try_map_identities(
                |_| Err::<TypeId, _>("type identity"),
                |_| Ok::<_, &'static str>(task.fields()[0].id()),
                |_| Ok::<_, &'static str>(function.id()),
                |_| Ok::<_, &'static str>(function.parameters()[0].id()),
            )
            .unwrap_err(),
        "type identity"
    );
    assert_eq!(
        checked_plan
            .try_map_identities(
                |_| Ok::<_, &'static str>(task.id()),
                |_| Err::<FieldId, _>("field identity"),
                |_| Ok::<_, &'static str>(function.id()),
                |_| Ok::<_, &'static str>(function.parameters()[0].id()),
            )
            .unwrap_err(),
        "field identity"
    );
    assert_eq!(
        checked_plan
            .try_map_identities(
                |_| Ok::<_, &'static str>(task.id()),
                |_| Ok::<_, &'static str>(task.fields()[0].id()),
                |_| Err::<FunctionId, _>("function identity"),
                |_| Ok::<_, &'static str>(function.parameters()[0].id()),
            )
            .unwrap_err(),
        "function identity"
    );
    assert_eq!(
        checked_plan
            .try_map_identities(
                |_| Ok::<_, &'static str>(task.id()),
                |_| Ok::<_, &'static str>(task.fields()[0].id()),
                |_| Ok::<_, &'static str>(function.id()),
                |_| Err::<ParameterId, _>("parameter identity"),
            )
            .unwrap_err(),
        "parameter identity"
    );
}

#[test]
fn identity_selected_validator_rejects_multi_hop_catalogue_mismatches() {
    let active = empty_active();
    let prepared = prepare(
        &checked_report(
            IDENTITY_SELECTED_NULLABLE_EQUALITY_SOURCE,
            active.catalogue(),
        ),
        active.pair(),
        &active,
    )
    .unwrap();
    let task = prepared
        .candidate()
        .object_type_by_name(&semantic_name(&["tasks", "task"]))
        .unwrap()
        .clone();
    let person = prepared
        .candidate()
        .object_type_by_name(&semantic_name(&["tasks", "person"]))
        .unwrap()
        .clone();
    let function = prepared.candidate().functions()[0].clone();
    let checked = checked_report(
        IDENTITY_SELECTED_NULLABLE_EQUALITY_SOURCE,
        active.catalogue(),
    );
    let checked_plan = checked.checked_bundle().unwrap().server_functions()[0]
        .identity_selected_query_plan()
        .unwrap();
    let owner_field = task.field_by_name("owner").unwrap();
    let name_field = person.field_by_name("name").unwrap();
    let map_plan = |type_ids: [TypeId; 7]| {
        let mut type_index = 0;
        let mut field_index = 0;
        let field_ids = [
            owner_field.id(),
            name_field.id(),
            owner_field.id(),
            name_field.id(),
        ];
        let plan = checked_plan
            .try_map_identities(
                |_| {
                    let mapped = type_ids[type_index];
                    type_index += 1;
                    Ok::<_, PrepareError>(mapped)
                },
                |_| {
                    let mapped = field_ids[field_index];
                    field_index += 1;
                    Ok::<_, PrepareError>(mapped)
                },
                |_| Ok::<_, PrepareError>(function.id()),
                |_| Ok::<_, PrepareError>(function.parameters()[0].id()),
            )
            .unwrap();
        assert_eq!(type_index, type_ids.len());
        assert_eq!(field_index, field_ids.len());
        plan
    };
    let exact_types = [
        task.id(),
        task.id(),
        person.id(),
        task.id(),
        person.id(),
        task.id(),
        person.id(),
    ];
    let plan = map_plan(exact_types);
    let references = identity_selected_query_reference_sequence(&plan, &function);
    let non_reference_task = ObjectTypeDefinition::new(
        task.id(),
        task.name().clone(),
        vec![FieldDefinition::new(
            owner_field.id(),
            owner_field.name(),
            owner_field.ordinal(),
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            owner_field.nullable(),
            false,
            None,
            None,
        )],
    );
    assert!(matches!(
        identity_selected_query_plan(
            &plan,
            &function,
            &[non_reference_task, person.clone()],
            &references,
        ),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "identity-selected query field path continues through a non-reference field"
        })
    ));

    let wrong_owner_plan = map_plan([
        task.id(),
        person.id(),
        person.id(),
        task.id(),
        person.id(),
        task.id(),
        person.id(),
    ]);
    assert!(matches!(
        identity_selected_query_plan(
            &wrong_owner_plan,
            &function,
            &[task, person],
            &identity_selected_query_reference_sequence(&wrong_owner_plan, &function),
        ),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "identity-selected query field path owner differs from its source object"
        })
    ));
}

#[test]
fn prepares_a_complete_server_mutation_artifact_and_reuses_only_equal_semantics() {
    let empty = empty_active();
    let initial = prepare(
        &checked_report(MUTATION_SOURCE, empty.catalogue()),
        empty.pair(),
        &empty,
    )
    .unwrap();

    let catalogue = initial.candidate();
    let task = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "task"]))
        .unwrap();
    let person = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "person"]))
        .unwrap();
    let function = &catalogue.functions()[0];
    let revision = &initial.new_function_revisions()[0];
    assert_eq!(revision.artifact().kind(), ExecutableArtifactKind::Server);
    assert_eq!(revision.artifact().format(), SERVER_MUTATION_PLAN_FORMAT);
    assert_eq!(
        revision.artifact().version(),
        orna_artifact::server_mutation_plan::INSERT_FORMAT_VERSION
    );
    assert_eq!(
        revision.language_version(),
        SERVER_MUTATION_PLAN_LANGUAGE_VERSION
    );
    assert_eq!(
        artifact_payload_digest(revision.artifact().payload()).unwrap(),
        revision.artifact().content_hash()
    );

    let plan = ServerMutationPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(plan.target(), task.id());
    assert_eq!(plan.returned_object(), task.id());
    assert_eq!(plan.assignments().len(), 4);
    assert_eq!(plan.assignments()[0].owner(), task.id());
    assert_eq!(plan.assignments()[0].field(), task.fields()[0].id());
    assert_eq!(plan.assignments()[1].field(), task.fields()[1].id());
    assert_eq!(plan.assignments()[2].field(), task.fields()[2].id());
    assert_eq!(plan.assignments()[3].field(), task.fields()[3].id());
    assert!(
        plan.assignments()
            .iter()
            .all(|assignment| assignment.owner() == task.id())
    );
    assert_eq!(
        plan.assignments()[0].expression().resolved_type(),
        ResolvedType::scalar(StandardScalar::CharacterLargeObject)
    );
    assert!(!plan.assignments()[0].expression().nullable());
    assert_eq!(
        plan.assignments()[1].expression().resolved_type(),
        ResolvedType::scalar(StandardScalar::Boolean)
    );
    assert!(!plan.assignments()[1].expression().nullable());
    assert_eq!(
        plan.assignments()[2].expression().resolved_type(),
        ResolvedType::scalar(StandardScalar::CharacterLargeObject)
    );
    assert_eq!(
        plan.assignments()[3].expression().resolved_type(),
        ResolvedType::reference(person.id())
    );
    assert!(!plan.assignments()[3].expression().nullable());
    assert!(matches!(
        plan.assignments()[0].expression().kind(),
        DurableMutationExpressionKind::Parameter { owner, parameter }
            if *owner == function.id() && *parameter == function.parameters()[0].id()
    ));
    assert!(matches!(
        plan.assignments()[1].expression().kind(),
        DurableMutationExpressionKind::BooleanLiteral { value: false }
    ));
    assert!(plan.assignments()[2].expression().nullable());
    assert!(matches!(
        plan.assignments()[2].expression().kind(),
        DurableMutationExpressionKind::TypedNull
    ));
    assert!(matches!(
        plan.assignments()[3].expression().kind(),
        DurableMutationExpressionKind::Parameter { owner, parameter }
            if *owner == function.id() && *parameter == function.parameters()[2].id()
    ));
    assert_eq!(
        initial
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(person.id())
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id())
            ),
            (
                DefinitionReferenceKind::WriteObject,
                DefinitionReferenceTarget::ObjectType(task.id())
            ),
            (
                DefinitionReferenceKind::WriteField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.fields()[0].id()
                }
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter {
                    owner: function.id(),
                    parameter: function.parameters()[0].id()
                }
            ),
            (
                DefinitionReferenceKind::WriteField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.fields()[1].id()
                }
            ),
            (
                DefinitionReferenceKind::WriteField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.fields()[2].id()
                }
            ),
            (
                DefinitionReferenceKind::WriteField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.fields()[3].id()
                }
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter {
                    owner: function.id(),
                    parameter: function.parameters()[2].id()
                }
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id())
            ),
        ]
    );
    assert_eq!(
        initial.references()[2].source_origin().byte_start() as usize,
        MUTATION_SOURCE.rfind("tasks.task AS created").unwrap()
    );
    assert_eq!(
        initial
            .references()
            .iter()
            .map(|reference| reference.ordinal())
            .collect::<Vec<_>>(),
        (0..10).collect::<Vec<_>>()
    );
    assert!(initial.references().iter().all(|reference| {
        reference.source_function() == function.id() && reference.source_revision() == revision.id()
    }));

    let active = activate(&initial, vec![revision.clone()], Vec::new());
    let reformatted = prepare(
        &checked_report(MUTATION_REFORMATTED_SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();
    assert!(reformatted.new_function_revisions().is_empty());
    assert_eq!(
        reformatted.candidate().functions()[0].current_revision(),
        revision.id()
    );

    let changed = prepare(
        &checked_report(MUTATION_CHANGED_SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();
    assert_eq!(changed.new_function_revisions().len(), 1);
    assert_ne!(changed.new_function_revisions()[0].id(), revision.id());
    assert_ne!(
        changed.new_function_revisions()[0].semantic_hash(),
        revision.semantic_hash()
    );
}

#[test]
fn prepares_update_version_two_with_selector_and_exact_references() {
    let empty = empty_active();
    let prepared = prepare(
        &checked_report(UPDATE_SOURCE, empty.catalogue()),
        empty.pair(),
        &empty,
    )
    .unwrap();

    let catalogue = prepared.candidate();
    let person = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "person"]))
        .unwrap();
    let task = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "task"]))
        .unwrap();
    let function = &catalogue.functions()[0];
    let revision = &prepared.new_function_revisions()[0];
    assert_eq!(
        revision.artifact().version(),
        orna_artifact::server_mutation_plan::UPDATE_FORMAT_VERSION
    );
    assert_eq!(
        artifact_payload_digest(revision.artifact().payload()).unwrap(),
        revision.artifact().content_hash()
    );
    let plan = ServerMutationPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(plan.format_version(), 2);
    assert_eq!(plan.target(), task.id());
    assert_eq!(plan.returned_object(), task.id());
    assert_eq!(plan.assignments().len(), 2);
    assert_eq!(plan.assignments()[0].field(), task.fields()[0].id());
    assert_eq!(plan.assignments()[1].field(), task.fields()[2].id());
    assert_eq!(
        plan.operation(),
        &ServerMutationOperation::Update {
            selector: orna_artifact::server_mutation_plan::MutationSelector::new(
                function.id(),
                function.parameters()[0].id(),
            )
        }
    );
    assert_eq!(
        prepared
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(person.id()),
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::WriteObject,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::WriteField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.fields()[0].id(),
                },
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter {
                    owner: function.id(),
                    parameter: function.parameters()[1].id(),
                },
            ),
            (
                DefinitionReferenceKind::WriteField,
                DefinitionReferenceTarget::Field {
                    owner: task.id(),
                    field: task.fields()[2].id(),
                },
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter {
                    owner: function.id(),
                    parameter: function.parameters()[2].id(),
                },
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter {
                    owner: function.id(),
                    parameter: function.parameters()[0].id(),
                },
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(task.id()),
            ),
        ]
    );
    assert_eq!(
        prepared
            .references()
            .iter()
            .map(|reference| reference.ordinal())
            .collect::<Vec<_>>(),
        (0..11).collect::<Vec<_>>()
    );
}

#[test]
fn prepares_delete_version_three_with_boolean_result_and_exact_references() {
    let empty = empty_active();
    let prepared = prepare(
        &checked_report(DELETE_SOURCE, empty.catalogue()),
        empty.pair(),
        &empty,
    )
    .unwrap();

    let catalogue = prepared.candidate();
    let target = catalogue
        .object_type_by_name(&semantic_name(&["tasks", "task"]))
        .unwrap();
    let function = &catalogue.functions()[0];
    let revision = &prepared.new_function_revisions()[0];
    assert_eq!(revision.artifact().kind(), ExecutableArtifactKind::Server);
    assert_eq!(revision.artifact().format(), SERVER_MUTATION_PLAN_FORMAT);
    assert_eq!(
        revision.artifact().version(),
        orna_artifact::server_mutation_plan::DELETE_FORMAT_VERSION
    );
    assert_eq!(
        artifact_payload_digest(revision.artifact().payload()).unwrap(),
        revision.artifact().content_hash()
    );
    assert_eq!(
        revision.language_version(),
        SERVER_MUTATION_PLAN_LANGUAGE_VERSION
    );
    let plan = ServerDeletePlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(plan.target(), target.id());
    assert_eq!(plan.selector().owner(), function.id());
    assert_eq!(plan.selector().parameter(), function.parameters()[0].id());
    assert!(matches!(
        function.return_type(),
        FunctionReturn::Rows(columns)
            if columns.len() == 1
                && columns[0].resolved_type()
                    == ResolvedType::Scalar(StandardScalar::Boolean)
    ));
    assert_eq!(
        prepared
            .references()
            .iter()
            .map(|reference| (reference.kind(), reference.target()))
            .collect::<Vec<_>>(),
        vec![
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(target.id()),
            ),
            (
                DefinitionReferenceKind::WriteObject,
                DefinitionReferenceTarget::ObjectType(target.id()),
            ),
            (
                DefinitionReferenceKind::ObjectReference,
                DefinitionReferenceTarget::ObjectType(target.id()),
            ),
            (
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter {
                    owner: function.id(),
                    parameter: function.parameters()[0].id(),
                },
            ),
        ]
    );
    assert_eq!(
        prepared
            .references()
            .iter()
            .map(|reference| {
                (
                    reference.ordinal(),
                    reference.source_origin().byte_start() as usize,
                    reference.source_origin().byte_end() as usize,
                )
            })
            .collect::<Vec<_>>(),
        [
            (0, "p_task REF ", "tasks.task"),
            (1, "DELETE FROM ", "tasks.task"),
            (2, "WHERE REF(", "removed"),
            (3, "= ", "p_task"),
        ]
        .into_iter()
        .zip([
            "p_task REF tasks.task",
            "DELETE FROM tasks.task",
            "WHERE REF(removed)",
            "= p_task RETURNING",
        ])
        .map(|((ordinal, prefix, token), context)| {
            let start = DELETE_SOURCE.find(context).unwrap() + prefix.len();
            (ordinal, start, start + token.len())
        })
        .collect::<Vec<_>>()
    );
}

#[test]
fn mutation_preparation_revalidates_durable_catalogue_and_reference_facts() {
    let target_id = TypeId::from_bytes([41; 16]);
    let title_id = FieldId::from_bytes([42; 16]);
    let note_id = FieldId::from_bytes([43; 16]);
    let function_id = FunctionId::from_bytes([44; 16]);
    let parameter_id = ParameterId::from_bytes([45; 16]);
    let text = ResolvedType::scalar(StandardScalar::CharacterLargeObject);
    let target = ObjectTypeDefinition::new(
        target_id,
        semantic_name(&["tasks", "task"]),
        vec![
            FieldDefinition::new(title_id, "title", 0, text, false, false, None, None),
            FieldDefinition::new(note_id, "note", 1, text, true, false, None, None),
        ],
    );
    let function = FunctionDefinition::new(
        function_id,
        semantic_name(&["tasks", "create"]),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            parameter_id,
            "title",
            0,
            text,
            None,
        )],
        FunctionReturn::Rows(Vec::new()),
        FunctionRevisionId::from_bytes([46; 16]),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Volatile,
    );
    let parameter = MutationAssignment::new(
        target_id,
        title_id,
        MutationExpression::new(
            MutationExpressionKind::ParameterRead {
                owner: function_id,
                parameter: parameter_id,
            },
            MutationValueType::new(
                SemanticType::scalar(StandardScalar::CharacterLargeObject),
                false,
            ),
        ),
    );
    assert!(
        validate_mutation_assignments(std::slice::from_ref(&parameter), &target, &function, true,)
            .is_ok()
    );

    let cross_owner = MutationAssignment::new(
        TypeId::from_bytes([47; 16]),
        title_id,
        parameter.expression().clone(),
    );
    let unknown_field = MutationAssignment::new(
        target_id,
        FieldId::from_bytes([48; 16]),
        parameter.expression().clone(),
    );
    let wrong_field_type = MutationAssignment::new(
        target_id,
        title_id,
        MutationExpression::new(
            MutationExpressionKind::BooleanLiteral { value: true },
            MutationValueType::new(SemanticType::scalar(StandardScalar::Boolean), false),
        ),
    );
    let wrong_parameter_type = MutationAssignment::new(
        target_id,
        title_id,
        MutationExpression::new(
            MutationExpressionKind::ParameterRead {
                owner: function_id,
                parameter: parameter_id,
            },
            MutationValueType::new(
                SemanticType::scalar(StandardScalar::CharacterLargeObject),
                false,
            ),
        ),
    );
    let function_with_wrong_parameter_type = FunctionDefinition::new(
        function_id,
        semantic_name(&["tasks", "create"]),
        FunctionDomain::Server,
        vec![ParameterDefinition::new(
            parameter_id,
            "title",
            0,
            ResolvedType::scalar(StandardScalar::Integer),
            None,
        )],
        FunctionReturn::Rows(Vec::new()),
        FunctionRevisionId::from_bytes([46; 16]),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Volatile,
    );
    let nullable_null = MutationAssignment::new(
        target_id,
        title_id,
        MutationExpression::new(
            MutationExpressionKind::TypedNull,
            MutationValueType::new(
                SemanticType::scalar(StandardScalar::CharacterLargeObject),
                true,
            ),
        ),
    );
    for assignments in [
        vec![cross_owner],
        vec![unknown_field],
        vec![wrong_field_type],
        vec![nullable_null],
        Vec::new(),
    ] {
        assert!(validate_mutation_assignments(&assignments, &target, &function, true).is_err());
    }
    assert!(matches!(
        validate_mutation_assignments(
            &[wrong_parameter_type],
            &target,
            &function_with_wrong_parameter_type,
            true,
        ),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation parameter type differs from its expression"
        })
    ));

    let expected = vec![
        (
            DefinitionReferenceKind::WriteObject,
            DefinitionReferenceTarget::ObjectType(target_id),
        ),
        (
            DefinitionReferenceKind::WriteField,
            DefinitionReferenceTarget::Field {
                owner: target_id,
                field: title_id,
            },
        ),
    ];
    assert!(
        validate_reference_sequence(
            &expected,
            &expected,
            "mutation definition references differ from the checked body"
        )
        .is_ok()
    );
    let mut reordered = expected.clone();
    reordered.reverse();
    assert!(
        validate_reference_sequence(
            &expected,
            &reordered,
            "mutation definition references differ from the checked body"
        )
        .is_err()
    );
    assert!(
        validate_reference_sequence(
            &expected,
            &expected[..1],
            "mutation definition references differ from the checked body"
        )
        .is_err()
    );
}

#[test]
fn record_constructor_preparation_rejects_a_nullable_object_field() {
    let boolean_id = TypeId::from_bytes([0x91; 16]);
    let record_id = TypeId::from_bytes([0x92; 16]);
    let record_field_id = FieldId::from_bytes([0x93; 16]);
    let standard = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0x94; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x95; 16]),
            semantic_name(&["std"]),
        )],
        vec![],
        vec![orna_core::catalogue::ValueTypeDefinition::primitive(
            boolean_id,
            semantic_name(&["std", "boolean"]),
            ValueTypeMutability::Immutable,
            ValueTypePersistence::Persistable,
            "orna.kernel.value.boolean@1",
        )],
        vec![],
    )
    .unwrap();
    let record = RecordValueTypeDefinition::new(
        record_id,
        semantic_name(&["tasks", "flags"]),
        vec![
            RecordValueFieldDefinition::try_new_descriptor(
                record_field_id,
                "active",
                0,
                TypeDescriptor::named(boolean_id),
            )
            .unwrap(),
        ],
    );
    let target_field = FieldDefinition::new(
        FieldId::from_bytes([0x96; 16]),
        "flags",
        0,
        ResolvedType::named(record_id),
        true,
        false,
        None,
        None,
    );
    let function = FunctionDefinition::new(
        FunctionId::from_bytes([0x97; 16]),
        semantic_name(&["tasks", "create"]),
        FunctionDomain::Server,
        vec![],
        FunctionReturn::Rows(vec![]),
        FunctionRevisionId::from_bytes([0x98; 16]),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::Atomic),
        FunctionVolatility::Volatile,
    );
    let boolean_type = MutationValueType::new(SemanticType::scalar(StandardScalar::Boolean), false)
        .with_standard_value_type(boolean_id);
    let expression = MutationExpression::new(
        MutationExpressionKind::RecordConstructor {
            record_type: record_id,
            fields: vec![MutationRecordFieldExpression::new(
                record_id,
                record_field_id,
                MutationRecordFieldExpressionKind::BooleanLiteral { value: true },
                boolean_type,
            )],
        },
        MutationValueType::new(SemanticType::Named(record_id), false),
    );

    assert!(matches!(
        server_mutation_expression(
            &expression,
            &function,
            &target_field,
            &[],
            std::slice::from_ref(&record),
            Some(&standard),
        ),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "record constructor targets a nullable object field"
        })
    ));
}

#[test]
fn mutation_parameter_validation_rejects_unused_unsupported_types_and_defaults() {
    let function_id = FunctionId::from_bytes([51; 16]);
    let valid_parameter_id = ParameterId::from_bytes([52; 16]);
    let unused_parameter_id = ParameterId::from_bytes([53; 16]);
    let function_with_unused = |resolved_type, default_expression| {
        FunctionDefinition::new(
            function_id,
            semantic_name(&["tasks", "create"]),
            FunctionDomain::Server,
            vec![
                ParameterDefinition::new(
                    valid_parameter_id,
                    "used_title",
                    0,
                    ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    None,
                ),
                ParameterDefinition::new(
                    unused_parameter_id,
                    "unused",
                    1,
                    resolved_type,
                    default_expression,
                ),
            ],
            FunctionReturn::Rows(Vec::new()),
            FunctionRevisionId::from_bytes([54; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        )
    };

    let unsupported = function_with_unused(ResolvedType::scalar(StandardScalar::Decimal), None);
    assert!(matches!(
        validate_mutation_parameters(&unsupported, &[]),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation parameter has an unsupported runtime type"
        })
    ));

    let defaulted = function_with_unused(
        ResolvedType::scalar(StandardScalar::Integer),
        Some(ExpressionId::from_bytes([55; 16])),
    );
    assert!(matches!(
        validate_mutation_parameters(&defaulted, &[]),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation parameter has an unsupported default expression"
        })
    ));
}

#[test]
fn mutation_selector_validation_requires_exact_owner_parameter_and_target() {
    let function_id = FunctionId::from_bytes([61; 16]);
    let parameter_id = ParameterId::from_bytes([62; 16]);
    let target = TypeId::from_bytes([63; 16]);
    let function_with = |resolved_type| {
        FunctionDefinition::new(
            function_id,
            semantic_name(&["tasks", "update"]),
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                parameter_id,
                "selected",
                0,
                resolved_type,
                None,
            )],
            FunctionReturn::Rows(Vec::new()),
            FunctionRevisionId::from_bytes([64; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        )
    };
    let valid = function_with(ResolvedType::reference(target));
    assert!(validate_mutation_selector(function_id, parameter_id, target, &valid).is_ok());
    assert!(matches!(
        validate_mutation_selector(
            FunctionId::from_bytes([65; 16]),
            parameter_id,
            target,
            &valid,
        ),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation selector owner differs from its enclosing function"
        })
    ));
    assert!(matches!(
        validate_mutation_selector(
            function_id,
            ParameterId::from_bytes([66; 16]),
            target,
            &valid,
        ),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation selector parameter is not declared by its enclosing function"
        })
    ));
    let wrong_type = function_with(ResolvedType::scalar(StandardScalar::BigInt));
    assert!(matches!(
        validate_mutation_selector(function_id, parameter_id, target, &wrong_type),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation selector parameter does not reference its target object"
        })
    ));
    let wrong_target = function_with(ResolvedType::reference(TypeId::from_bytes([67; 16])));
    assert!(matches!(
        validate_mutation_selector(function_id, parameter_id, target, &wrong_target),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation selector parameter does not reference its target object"
        })
    ));
}

#[test]
fn delete_preparation_revalidates_target_modes_result_and_evidence() {
    let function_id = FunctionId::from_bytes([71; 16]);
    let parameter_id = ParameterId::from_bytes([72; 16]);
    let target_id = TypeId::from_bytes([73; 16]);
    let revision_id = FunctionRevisionId::from_bytes([74; 16]);
    let target =
        ObjectTypeDefinition::new(target_id, semantic_name(&["tasks", "task"]), Vec::new());
    let function_with = |return_type, security| {
        FunctionDefinition::new(
            function_id,
            semantic_name(&["tasks", "remove"]),
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                parameter_id,
                "p_task",
                0,
                ResolvedType::reference(target_id),
                None,
            )],
            return_type,
            revision_id,
            security,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        )
    };
    let boolean_rows = || {
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "deleted",
            0,
            ResolvedType::scalar(StandardScalar::Boolean),
        )])
    };
    let function = function_with(boolean_rows(), FunctionSecurity::Invoker);
    let plan = DeletePlanIr::new(target_id, function_id, parameter_id);
    let references = delete_reference_sequence(&plan, &function);

    assert!(
        server_delete_plan(&plan, &function, std::slice::from_ref(&target), &references).is_ok()
    );
    assert!(matches!(
        server_delete_plan(&plan, &function, &[], &references),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "DELETE target object is absent from the candidate catalogue"
        })
    ));

    let definer = function_with(boolean_rows(), FunctionSecurity::Definer);
    assert!(matches!(
        server_delete_plan(
            &plan,
            &definer,
            std::slice::from_ref(&target),
            &delete_reference_sequence(&plan, &definer),
        ),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "DELETE function has unsupported execution modes"
        })
    ));

    let wrong_result = function_with(
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "deleted",
            0,
            ResolvedType::scalar(StandardScalar::Integer),
        )]),
        FunctionSecurity::Invoker,
    );
    assert!(matches!(
        server_delete_plan(
            &plan,
            &wrong_result,
            std::slice::from_ref(&target),
            &delete_reference_sequence(&plan, &wrong_result),
        ),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "DELETE function does not return exactly one BOOLEAN column"
        })
    ));

    assert!(matches!(
        server_delete_plan(
            &plan,
            &function,
            std::slice::from_ref(&target),
            &references[..references.len() - 1],
        ),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "mutation definition references differ from the checked body"
        })
    ));
}

#[test]
fn allocates_fresh_candidate_revisions_for_repeated_preparation() {
    let active = empty_active();
    let report = checked_report(SOURCE, active.catalogue());

    let first = prepare(&report, active.pair(), &active).unwrap();
    let second = prepare(&report, active.pair(), &active).unwrap();

    assert_ne!(first.candidate_pair(), second.candidate_pair());
    assert_ne!(first.source().bundle(), second.source().bundle());
    assert_ne!(
        first.source().units()[0].id(),
        second.source().units()[0].id()
    );
    assert_ne!(
        first.candidate().object_types()[0].id(),
        second.candidate().object_types()[0].id()
    );
}

#[test]
fn prepares_and_replays_required_unique_references_fail_closed() {
    let empty = empty_active();
    let report = checked_report(REQUIRED_UNIQUE_REFERENCE_SOURCE, empty.catalogue());
    let checked = report.checked_bundle().unwrap();
    let checked_assignment = checked
        .object_types()
        .iter()
        .find(|object_type| object_type.name() == &semantic_name(&["relations", "assignment"]))
        .unwrap();
    let checked_field = &checked_assignment.fields()[0];
    let checked_owner = checked
        .object_types()
        .iter()
        .find(|object_type| object_type.name() == &semantic_name(&["relations", "owner"]))
        .unwrap()
        .id();

    let prepared = prepare(&report, empty.pair(), &empty).unwrap();
    let assignment = prepared
        .candidate()
        .object_type_by_name(&semantic_name(&["relations", "assignment"]))
        .unwrap();
    let owner = prepared
        .candidate()
        .object_type_by_name(&semantic_name(&["relations", "owner"]))
        .unwrap();
    let field = assignment.field_by_name("owner").unwrap();
    assert!(field.unique());
    assert!(!field.nullable());
    assert_eq!(field.resolved_type(), ResolvedType::reference(owner.id()));

    let field_id = field.id();
    let assignment_id = assignment.id();
    let owner_id = owner.id();
    let active = activate(&prepared, Vec::new(), Vec::new());
    let replay = prepare(
        &checked_report(REQUIRED_UNIQUE_REFERENCE_SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();
    let replay_assignment = replay
        .candidate()
        .object_type_by_name(&semantic_name(&["relations", "assignment"]))
        .unwrap();
    let replay_owner = replay
        .candidate()
        .object_type_by_name(&semantic_name(&["relations", "owner"]))
        .unwrap();
    let replay_field = replay_assignment.field_by_name("owner").unwrap();
    assert_eq!(replay_assignment.id(), assignment_id);
    assert_eq!(replay_owner.id(), owner_id);
    assert_eq!(replay_field.id(), field_id);
    assert!(replay_field.unique());
    assert_eq!(replay_field.resolved_type(), field.resolved_type());

    for (semantic_type, nullable) in [
        (SemanticType::scalar(StandardScalar::Boolean), false),
        (SemanticType::reference(checked_owner), true),
    ] {
        let mut malformed = report.clone();
        assert!(malformed.replace_checked_field_facts_for_test(
            checked_assignment.id(),
            checked_field.id(),
            semantic_type,
            nullable,
            true,
        ));
        assert_preparation_reason(
            prepare(&malformed, empty.pair(), &empty),
            UNIQUE_FIELD_MESSAGE,
        );
    }
}

#[test]
fn prepares_unique_text_fields_and_rejects_hostile_checked_shapes() {
    let empty = empty_active();
    let report = checked_report(UNIQUE_TEXT_SOURCE, empty.catalogue());
    assert!(report.diagnostics().is_empty());
    let checked = report.checked_bundle().unwrap();
    let checked_contact = checked
        .object_types()
        .iter()
        .find(|object_type| object_type.name() == &semantic_name(&["crm", "contact"]))
        .unwrap();
    let checked_email = &checked_contact.fields()[0];

    let prepared = prepare(&report, empty.pair(), &empty).unwrap();
    let contact = prepared
        .candidate()
        .object_type_by_name(&semantic_name(&["crm", "contact"]))
        .unwrap();
    let email = contact.field_by_name("email").unwrap();
    let name = contact.field_by_name("name").unwrap();
    assert!(email.unique());
    assert!(email.nullable());
    assert_eq!(
        email.resolved_type(),
        ResolvedType::scalar(StandardScalar::CharacterLargeObject)
    );
    assert!(name.unique());
    assert!(!name.nullable());
    assert_eq!(
        name.resolved_type(),
        ResolvedType::scalar(StandardScalar::CharacterLargeObject)
    );

    for (semantic_type, nullable) in [
        (SemanticType::scalar(StandardScalar::Boolean), false),
        (SemanticType::reference(checked_contact.id()), true),
        (SemanticType::Named(checked_contact.id()), false),
    ] {
        let mut hostile = report.clone();
        assert!(hostile.replace_checked_field_facts_for_test(
            checked_contact.id(),
            checked_email.id(),
            semantic_type,
            nullable,
            true,
        ));
        assert_preparation_reason(
            prepare(&hostile, empty.pair(), &empty),
            UNIQUE_FIELD_MESSAGE,
        );
    }
}

#[test]
fn durable_unique_field_support_requires_exact_legacy_or_standard_text_authority() {
    let text_id = TypeId::from_bytes([0xa1; 16]);
    let other_id = TypeId::from_bytes([0xa2; 16]);
    let transient_id = TypeId::from_bytes([0xa3; 16]);
    let opaque_id = TypeId::from_bytes([0xa4; 16]);
    let accepted = ValueTypeDefinition::primitive(
        text_id,
        semantic_name(&["std", "types", "text"]),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.character-large-object@1",
    );
    let other_contract = ValueTypeDefinition::primitive(
        other_id,
        semantic_name(&["std", "types", "other"]),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "other@1",
    );
    let transient = ValueTypeDefinition::primitive(
        transient_id,
        semantic_name(&["std", "types", "transient"]),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Transient,
        "orna.kernel.value.character-large-object@1",
    );
    let opaque = ValueTypeDefinition::opaque(
        opaque_id,
        semantic_name(&["std", "types", "opaque"]),
        "orna.kernel.value.character-large-object@1",
    );
    let standard = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0xa5; 16]),
        vec![
            SchemaDefinition::new(SchemaId::from_bytes([0xa8; 16]), semantic_name(&["std"])),
            SchemaDefinition::new(
                SchemaId::from_bytes([0xa9; 16]),
                semantic_name(&["std", "types"]),
            ),
        ],
        Vec::new(),
        vec![accepted, other_contract, transient, opaque],
        Vec::new(),
    )
    .expect("test standard catalogue must be valid");

    for nullable in [false, true] {
        assert!(supports_durable_unique_field(
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            nullable,
            None,
        ));
        assert!(supports_durable_unique_field(
            ResolvedType::value(text_id),
            nullable,
            Some(&standard),
        ));
    }
    assert!(!supports_durable_unique_field(
        ResolvedType::value(text_id),
        false,
        None,
    ));
    assert!(!supports_durable_unique_field(
        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
        false,
        Some(&standard),
    ));
    for type_id in [
        other_id,
        transient_id,
        opaque_id,
        TypeId::from_bytes([0xa6; 16]),
    ] {
        assert!(!supports_durable_unique_field(
            ResolvedType::value(type_id),
            false,
            Some(&standard),
        ));
    }
    let owner = TypeId::from_bytes([0xa7; 16]);
    assert!(supports_durable_unique_field(
        ResolvedType::reference(owner),
        false,
        None,
    ));
    assert!(supports_durable_unique_field(
        ResolvedType::reference(owner),
        false,
        Some(&standard),
    ));
    assert!(!supports_durable_unique_field(
        ResolvedType::reference(owner),
        true,
        None,
    ));
    assert!(!supports_durable_unique_field(
        ResolvedType::reference(owner),
        true,
        Some(&standard),
    ));
}

#[test]
fn preserves_complete_multi_unit_source_order_and_exact_bytes() {
    let active = empty_active();
    let first = "-- first\nCREATE SCHEMA multi;\n";
    let second = "-- second\r\nCREATE TYPE multi.item AS OBJECT (value INT);\r\n";
    let bundle = SourceBundle::new([
        SourceUnit::new("01-schema.orna", first),
        SourceUnit::new("02-type.orna", second),
    ])
    .unwrap();
    let report = check(&bundle, active.catalogue());

    let prepared = prepare(&report, active.pair(), &active).unwrap();

    assert_eq!(
        prepared
            .source()
            .units()
            .iter()
            .map(|unit| (unit.ordinal(), unit.logical_path(), unit.content()))
            .collect::<Vec<_>>(),
        vec![(0, "01-schema.orna", first), (1, "02-type.orna", second),]
    );
}

#[test]
fn rejects_incomplete_and_stale_inputs_before_preparation() {
    let active = empty_active();
    let failed = checked_report("CREATE SCHEMA ;", active.catalogue());
    assert!(matches!(
        prepare(&failed, active.pair(), &active),
        Err(PrepareError::CheckNotComplete {
            diagnostic_count: 1
        })
    ));

    let report = checked_report(SOURCE, active.catalogue());
    let stale_source = RevisionPair::new(SourceRevisionId::new(), active.pair().catalogue());
    assert!(matches!(
        prepare(&report, stale_source, &active),
        Err(PrepareError::ExpectedBaseMismatch { .. })
    ));
    let stale_catalogue = RevisionPair::new(active.pair().source(), CatalogueRevisionId::new());
    assert!(matches!(
        prepare(&report, stale_catalogue, &active),
        Err(PrepareError::ExpectedBaseMismatch { .. })
    ));

    let other_base = empty_active();
    let mismatched = checked_report(SOURCE, other_base.catalogue());
    assert!(matches!(
        prepare(&mismatched, active.pair(), &active),
        Err(PrepareError::CheckedBaseMismatch { .. })
    ));
}

#[test]
fn rejects_existing_identities_absent_from_the_exact_active_catalogue() {
    let active = empty_active();
    let schema_id = SchemaId::new();
    let false_base = CatalogueSnapshot::new(
        active.catalogue().revision(),
        vec![SchemaDefinition::new(schema_id, semantic_name(&["tasks"]))],
        Vec::new(),
    )
    .unwrap();
    let report = checked_report(SOURCE, &false_base);

    assert!(matches!(
        prepare(&report, active.pair(), &active),
        Err(PrepareError::ExistingDefinitionMismatch {
            definition: DefinitionIdentity::Schema(id),
        }) if id == schema_id
    ));
}

#[test]
fn retains_one_identical_artifact_for_a_shared_existing_expression() {
    let active = shared_expression_active();
    let report = checked_report(SHARED_EXPRESSION_SOURCE, active.catalogue());

    let prepared = prepare(&report, active.pair(), &active).unwrap();

    assert_eq!(prepared.expressions().len(), 1);
    let fields = prepared.candidate().object_types()[0].fields();
    assert_eq!(
        fields[0].default_expression(),
        fields[1].default_expression()
    );
    let expression_origins = prepared
        .origins()
        .iter()
        .filter(|origin| matches!(origin.identity(), DefinitionIdentity::Expression(_)))
        .count();
    assert_eq!(expression_origins, 1);

    let inconsistent = checked_report(
        "CREATE SCHEMA demo; CREATE TYPE demo.item AS OBJECT (\
             first INT DEFAULT 1, second INT DEFAULT 2);",
        active.catalogue(),
    );
    assert!(matches!(
        prepare(&inconsistent, active.pair(), &active),
        Err(PrepareError::InvalidCheckedBundle {
            reason: "shared checked expression has inconsistent values",
        })
    ));
}

#[test]
fn source_only_edits_reuse_the_immutable_function_revision() {
    let active = empty_active();
    let initial = prepare(
        &checked_report(SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();
    let current_revision = initial.new_function_revisions()[0].clone();
    let active = activate(&initial, vec![current_revision.clone()], Vec::new());
    let report = checked_report(REFORMATTED_SOURCE, active.catalogue());

    let prepared = prepare(&report, active.pair(), &active).unwrap();

    assert!(prepared.new_function_revisions().is_empty());
    assert_eq!(
        prepared.candidate().schemas()[0].id(),
        active.catalogue().schemas()[0].id()
    );
    for (candidate, previous) in prepared
        .candidate()
        .object_types()
        .iter()
        .zip(active.catalogue().object_types())
    {
        assert_eq!(candidate.id(), previous.id());
        assert_eq!(
            candidate
                .fields()
                .iter()
                .map(FieldDefinition::id)
                .collect::<Vec<_>>(),
            previous
                .fields()
                .iter()
                .map(FieldDefinition::id)
                .collect::<Vec<_>>()
        );
    }
    assert_eq!(
        prepared.candidate().functions()[0].current_revision(),
        current_revision.id()
    );
    let current_origin = prepared
        .origins()
        .iter()
        .find(|origin| {
            origin.identity() == DefinitionIdentity::Function(current_revision.function())
        })
        .unwrap()
        .source();
    assert_ne!(
        current_origin.source_unit(),
        current_revision.declaration_origin().source_unit()
    );
    assert_eq!(
        current_revision.declaration_content_hash(),
        active.function_revisions()[0].declaration_content_hash()
    );
    assert_eq!(
        catalogue_digest(
            prepared.candidate(),
            active.function_revisions(),
            prepared.expressions(),
            prepared.origins(),
            prepared.references(),
        )
        .unwrap(),
        prepared.catalogue_hash()
    );
}

#[test]
fn field_rename_preparation_preserves_field_and_function_identities_on_replay() {
    let original_source = "CREATE SCHEMA people;\nCREATE TYPE people.person AS OBJECT (email TEXT NOT NULL);\nCREATE SERVER FUNCTION people.list_emails() RETURNS ROWS (email TEXT) AS SELECT p.email FROM people.person p;\n";
    let renamed_source = "CREATE SCHEMA people;\nCREATE TYPE people.person AS OBJECT (primary_email TEXT NOT NULL);\nALTER TYPE people.person RENAME FIELD email TO primary_email;\nCREATE SERVER FUNCTION people.list_emails() RETURNS ROWS (email TEXT) AS SELECT p.primary_email FROM people.person p;\n";
    let empty = empty_active();
    let original = prepare(
        &checked_report(original_source, empty.catalogue()),
        empty.pair(),
        &empty,
    )
    .unwrap();
    let original_revision = original.new_function_revisions()[0].clone();
    let original_field = original.candidate().object_types()[0].fields()[0].id();
    let owner = original.candidate().object_types()[0].id();
    let active = activate(&original, vec![original_revision.clone()], Vec::new());

    let renamed = prepare(
        &checked_report(renamed_source, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();
    let field = &renamed.candidate().object_types()[0].fields()[0];
    assert_eq!(field.name(), "primary_email");
    assert_eq!(field.id(), original_field);
    let field_origin = renamed
        .origins()
        .iter()
        .find(|origin| {
            origin.identity()
                == DefinitionIdentity::Field {
                    owner,
                    field: original_field,
                }
        })
        .unwrap()
        .source();
    let create_field = renamed_source.find("primary_email TEXT").unwrap();
    assert_eq!(field_origin.byte_start() as usize, create_field);
    assert_eq!(
        field_origin.byte_end() as usize,
        create_field + "primary_email TEXT NOT NULL".len()
    );
    assert_ne!(
        field_origin.byte_start() as usize,
        renamed_source.find("TO primary_email").unwrap() + 3
    );
    let reference = renamed
        .references()
        .iter()
        .find(|reference| reference.kind() == DefinitionReferenceKind::QueryField)
        .unwrap();
    assert_eq!(
        reference.target(),
        DefinitionReferenceTarget::Field {
            owner,
            field: original_field
        }
    );
    let dependent_token = renamed_source.find("p.primary_email").unwrap() + 2;
    assert_eq!(
        reference.source_origin().byte_start() as usize,
        dependent_token
    );
    assert_eq!(
        reference.source_origin().byte_end() as usize,
        dependent_token + "primary_email".len()
    );
    assert_ne!(renamed.source().bundle(), active.source().bundle());
    assert_ne!(
        renamed.source().bundle_hash(),
        active.source().bundle_hash()
    );
    assert_ne!(
        renamed.source().revision_hash(),
        active.source().revision_hash()
    );
    assert_ne!(renamed.catalogue_hash(), active.catalogue_hash());
    assert!(renamed.new_function_revisions().is_empty());
    assert_eq!(
        renamed.candidate().functions()[0].current_revision(),
        original_revision.id()
    );
    assert_eq!(
        active.function_revisions(),
        std::slice::from_ref(&original_revision)
    );

    let replay_active = activate(&renamed, vec![original_revision.clone()], Vec::new());
    assert_eq!(
        replay_active.function_revisions(),
        std::slice::from_ref(&original_revision)
    );
    assert_eq!(
        replay_active.function_revisions()[0].artifact(),
        original_revision.artifact()
    );
    let replay = prepare(
        &checked_report(renamed_source, replay_active.catalogue()),
        replay_active.pair(),
        &replay_active,
    )
    .unwrap();
    assert_eq!(
        replay.candidate().object_types()[0].fields()[0].id(),
        original_field
    );
    assert_eq!(
        replay.candidate().functions()[0].current_revision(),
        original_revision.id()
    );
    assert!(replay.new_function_revisions().is_empty());
}

#[test]
fn changed_semantics_use_the_max_history_revision_number_plus_one() {
    let active = empty_active();
    let initial = prepare(
        &checked_report(SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();
    let current = initial.new_function_revisions()[0].clone();
    let history = FunctionRevisionRecord::new(
        current.function(),
        FunctionRevisionId::new(),
        7,
        SourceOrigin::new(SourceUnitId::new(), 0, 1).unwrap(),
        digest(71),
        digest(72),
        SERVER_PLAN_LANGUAGE_VERSION,
        current.artifact().clone(),
    )
    .unwrap();
    let active = activate(&initial, vec![current], vec![history]);

    let prepared = prepare(
        &checked_report(CHANGED_SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();

    assert_eq!(prepared.new_function_revisions().len(), 1);
    assert_eq!(prepared.new_function_revisions()[0].revision_number(), 8);
    assert_ne!(
        prepared.new_function_revisions()[0].semantic_hash(),
        active.function_revisions()[0].semantic_hash()
    );
}

#[test]
fn semantic_history_reuse_selects_the_lowest_matching_revision() {
    let empty = empty_active();
    let initial = prepare(
        &checked_report(SOURCE, empty.catalogue()),
        empty.pair(),
        &empty,
    )
    .unwrap();
    let old = initial.new_function_revisions()[0].clone();
    let active_v1 = activate(&initial, vec![old.clone()], Vec::new());
    let changed = prepare(
        &checked_report(CHANGED_SOURCE, active_v1.catalogue()),
        active_v1.pair(),
        &active_v1,
    )
    .unwrap();
    let current = changed.new_function_revisions()[0].clone();
    let equivalent_later = FunctionRevisionRecord::new(
        old.function(),
        FunctionRevisionId::new(),
        3,
        SourceOrigin::new(SourceUnitId::new(), 0, 1).unwrap(),
        digest(73),
        old.semantic_hash(),
        old.language_version(),
        old.artifact().clone(),
    )
    .unwrap();
    let active = activate(&changed, vec![current], vec![old.clone(), equivalent_later]);

    let prepared = prepare(
        &checked_report(REFORMATTED_SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap();

    assert!(prepared.new_function_revisions().is_empty());
    assert_eq!(
        prepared.candidate().functions()[0].current_revision(),
        old.id()
    );
}

#[test]
fn legacy_reuse_remains_semantic_hash_only() {
    let empty = empty_active();
    let initial = prepare(
        &checked_report(SOURCE, empty.catalogue()),
        empty.pair(),
        &empty,
    )
    .unwrap();
    let old = initial.new_function_revisions()[0].clone();
    let active_v1 = activate(&initial, vec![old.clone()], Vec::new());
    let changed = prepare(
        &checked_report(CHANGED_SOURCE, active_v1.catalogue()),
        active_v1.pair(),
        &active_v1,
    )
    .unwrap();
    let current = changed.new_function_revisions()[0].clone();
    let legacy_match = FunctionRevisionRecord::new(
        old.function(),
        old.id(),
        old.revision_number(),
        old.declaration_origin(),
        old.declaration_content_hash(),
        old.semantic_hash(),
        "legacy.claimed-language",
        old.artifact().clone(),
    )
    .unwrap();
    let active = activate(&changed, vec![current], vec![legacy_match.clone()]);

    let error = prepare(
        &checked_report(REFORMATTED_SOURCE, active.catalogue()),
        active.pair(),
        &active,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        PrepareError::CanonicalHash(CanonicalHashError::FunctionSemanticHashMismatch {
            function,
            revision,
        }) if function == legacy_match.function() && revision == legacy_match.id()
    ));
}

#[test]
fn standard_upgrade_gate_seven_retains_the_current_version_one_revision() {
    let empty = empty_active();
    let initial = prepare(
        &checked_report(SOURCE, empty.catalogue()),
        empty.pair(),
        &empty,
    )
    .unwrap();
    let first = initial.new_function_revisions()[0].clone();
    let first_active = activate(&initial, vec![first], Vec::new());
    let changed = prepare(
        &checked_report(CHANGED_SOURCE, first_active.catalogue()),
        first_active.pair(),
        &first_active,
    )
    .unwrap();
    let current = changed.new_function_revisions()[0].clone();
    let older_equal = FunctionRevisionRecord::new(
        current.function(),
        FunctionRevisionId::new(),
        1,
        SourceOrigin::new(SourceUnitId::new(), 0, 1).unwrap(),
        digest(74),
        current.semantic_hash(),
        current.language_version(),
        current.artifact().clone(),
    )
    .unwrap();
    let active = activate(&changed, vec![current.clone()], vec![older_equal.clone()]);
    let definition = active.catalogue().functions()[0].clone();
    let make_plan = |current_only| {
        FunctionRevisionPlan::new(
            &active,
            current.function(),
            FunctionRevisionPlanInput {
                semantic_hash_version: FunctionSemanticHashVersion::Version1,
                definition: &definition,
                language_version: current.language_version(),
                artifact: current.artifact(),
                expressions: active.expressions(),
                references: active.references(),
                current_only,
                reuse_policy: FunctionRevisionReusePolicy::Complete,
            },
        )
        .unwrap()
    };

    assert_eq!(
        make_plan(false).reusable.unwrap().id(),
        older_equal.id(),
        "the fixture must expose the historical rollback"
    );
    assert_eq!(
        make_plan(standard_upgrade_reuse_is_current_only(
            FunctionSemanticHashVersion::Version1,
        ))
        .reusable
        .unwrap()
        .id(),
        current.id()
    );
    assert!(!standard_upgrade_reuse_is_current_only(
        FunctionSemanticHashVersion::Version2
    ));
}

#[test]
fn empty_client_state_block_uses_expression_plan_format() {
    let verified = invocation_carrier_standard();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_standard_application_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let source = "CREATE SCHEMA examples; \
            CREATE CLIENT FUNCTION examples.ready() RETURNS BOOLEAN IS \
            BEGIN RETURN TRUE; END;";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    let revision = &prepared.new_function_revisions()[0];
    assert_eq!(
        revision.artifact().version(),
        CLIENT_PLAN_EXPRESSION_VERSION
    );
    let plan = ExpressionClientPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(
        plan.expression(),
        &ClientExpressionNode::Boolean { value: true },
    );
}

#[test]
fn accepted_client_action_preparation_preserves_durable_operation_identity_and_arguments() {
    let verified = action_standard();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_standard_application_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let source = r#"CREATE SCHEMA action_fixture;

CREATE CLIENT FUNCTION action_fixture.local(p_first TEXT, p_second TEXT)
    RETURNS TEXT AS p_first;

CREATE CLIENT FUNCTION action_fixture.call_local(p_first TEXT, p_second TEXT)
    RETURNS std.Action AS std.action.call(
        target => action_fixture.local,
        arguments => std.call.args(p_second => p_second, p_first => p_first)
    );"#;
    let bundle = SourceBundle::new([SourceUnit::new("action.orna", source)]).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert!(
        report.diagnostics().is_empty(),
        "accepted action source did not check: {:?}",
        report.diagnostics()
    );

    let checked_call_site = {
        let checked = report.preparation_view().unwrap().checked();
        let caller = checked
            .client_functions()
            .iter()
            .find(|function| function.name().parts() == ["action_fixture", "call_local"])
            .unwrap();
        let CheckedClientFunctionBody::Expression { expression } = caller.body() else {
            panic!("action client must use an expression body");
        };
        let CheckedClientExpression::Action { operation } = expression else {
            panic!("action client must retain its action operation");
        };
        operation.call_site()
    };

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    let target = prepared
        .candidate()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["action_fixture", "local"])
        .unwrap();
    let caller = prepared
        .candidate()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["action_fixture", "call_local"])
        .unwrap();
    let revision = prepared
        .new_function_revisions()
        .iter()
        .find(|revision| revision.function() == caller.id())
        .unwrap();
    assert_eq!(revision.artifact().version(), CLIENT_PLAN_ACTION_VERSION);

    let plan = ActionClientPlan::decode(revision.artifact().payload()).unwrap();
    let operation = plan.operation();
    assert_eq!(operation.domain(), ActionTargetDomain::Client);
    assert_eq!(operation.target(), target.id());
    assert_eq!(operation.target_revision(), prepared.candidate_pair());
    assert_eq!(operation.call_site(), checked_call_site);
    assert_ne!(operation.call_site().to_bytes(), [0; 16]);
    let text_type_id = verified
        .catalogue()
        .value_type_by_name(&semantic_name(&["std", "types", "character_large_object"]))
        .unwrap()
        .id();
    assert_eq!(operation.result_type(), text_type_id);

    let mut expected_arguments: Vec<_> = target
        .parameters()
        .iter()
        .map(|target_parameter| {
            let caller_parameter = caller
                .parameters()
                .iter()
                .find(|parameter| parameter.name() == target_parameter.name())
                .unwrap();
            (
                target_parameter.id(),
                ClientExpressionNode::ParameterRead {
                    parameter: caller_parameter.id(),
                },
            )
        })
        .collect();
    expected_arguments.sort_by_key(|(parameter, _)| *parameter);
    assert_eq!(operation.arguments(), expected_arguments.as_slice());
}

#[test]
fn named_standard_resource_result_uses_catalogue_value_identity() {
    let verified = action_standard();
    let text_type_id = verified
        .catalogue()
        .value_type_by_name(&semantic_name(&["std", "types", "character_large_object"]))
        .unwrap()
        .id();
    let action_type_id = verified
        .catalogue()
        .value_type_by_name(&semantic_name(&["std", "action", "action"]))
        .unwrap()
        .id();
    let base_active = empty_standard_application_active(&verified);
    let target_schema_id = SchemaId::from_bytes([0xc0; 16]);
    let target_id = FunctionId::from_bytes([0xc1; 16]);
    let target_first_parameter_id = ParameterId::from_bytes([0xc3; 16]);
    let target_second_parameter_id = ParameterId::from_bytes([0xc2; 16]);
    let target = FunctionDefinition::new(
        target_id,
        semantic_name(&["resource_catalogue", "forward"]),
        FunctionDomain::Server,
        vec![
            ParameterDefinition::new(
                target_first_parameter_id,
                "p_first",
                0,
                ResolvedType::Value(text_type_id),
                None,
            ),
            ParameterDefinition::new(
                target_second_parameter_id,
                "p_second",
                1,
                ResolvedType::Value(text_type_id),
                None,
            ),
        ],
        FunctionReturn::Single(ResolvedType::Named(action_type_id)),
        FunctionRevisionId::from_bytes([0xc4; 16]),
        FunctionSecurity::Invoker,
        Some(FunctionTransaction::ReadOnly),
        FunctionVolatility::Stable,
    );
    let target_artifact = ExecutableArtifact::new(
        ExecutableArtifactKind::Server,
        SERVER_PLAN_FORMAT,
        SERVER_PLAN_VERSION,
        vec![0],
        artifact_payload_digest(&[0]).unwrap(),
    )
    .unwrap();
    let target_semantic_hash = function_semantic_digest_with_version(
        FunctionSemanticHashVersion::Version1,
        &target,
        SERVER_PLAN_LANGUAGE_VERSION,
        &target_artifact,
        &[],
        &[],
    )
    .unwrap();
    let target_source_origin =
        SourceOrigin::new(base_active.source().units()[0].id(), 0, 0).unwrap();
    let target_revision = FunctionRevisionRecord::new(
        target_id,
        FunctionRevisionId::from_bytes([0xc4; 16]),
        1,
        target_source_origin,
        function_declaration_digest(b"resource_catalogue.forward").unwrap(),
        target_semantic_hash,
        SERVER_PLAN_LANGUAGE_VERSION,
        target_artifact,
    )
    .unwrap();
    let target_origins = vec![
        DefinitionOrigin::new(
            DefinitionIdentity::Schema(target_schema_id),
            target_source_origin,
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Function(target_id),
            target_source_origin,
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Parameter {
                owner: target_id,
                parameter: target_first_parameter_id,
            },
            target_source_origin,
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Parameter {
                owner: target_id,
                parameter: target_second_parameter_id,
            },
            target_source_origin,
        ),
    ];
    let catalogue = CatalogueSnapshot::new_with_functions_and_types(
        CatalogueRevisionId::from_bytes([0xc5; 16]),
        vec![SchemaDefinition::new(
            target_schema_id,
            semantic_name(&["resource_catalogue"]),
        )],
        Vec::new(),
        Vec::new(),
        Vec::new(),
        vec![target],
    )
    .unwrap();
    let hash_context = CatalogueHashContext::version_two(verified.clone());
    let catalogue_hash = catalogue_digest_with_context(
        &hash_context,
        &catalogue,
        std::slice::from_ref(&target_revision),
        &[],
        &target_origins,
        &[],
    )
    .unwrap();
    let active = ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            RevisionPair::new(base_active.source().id(), catalogue.revision()),
            base_active.source().clone(),
            catalogue,
            catalogue_hash,
            ActiveRevisionContent::new(
                Vec::new(),
                vec![target_revision],
                target_origins,
                Vec::new(),
            ),
        ),
        hash_context,
    )
    .unwrap();
    let standard = check_standard_library_source(&verified).unwrap();
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let source = r#"CREATE SCHEMA resource_fixture;

CREATE CLIENT FUNCTION resource_fixture.call(p_first TEXT, p_second TEXT)
RETURNS std.Action IS

BEGIN
    RETURN AWAIT std.data.resource(
        target => resource_catalogue.forward,
        arguments => std.call.args(p_second => p_second, p_first => p_first)
    );
END;"#;
    let bundle = SourceBundle::new([SourceUnit::new("named-resource.orna", source)]).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert!(
        report.diagnostics().is_empty(),
        "named resource source did not check: {:?}",
        report.diagnostics()
    );

    let checked = report.preparation_view().unwrap().checked();
    let checked_caller = checked
        .client_functions()
        .iter()
        .find(|function| function.name().parts() == ["resource_fixture", "call"])
        .unwrap();
    let checked_call_site = {
        let CheckedClientFunctionBody::Expression { expression } = checked_caller.body() else {
            panic!("named resource client must use an expression body");
        };
        let CheckedClientExpression::Await { expression, .. } = expression else {
            panic!("named resource client must await its resource");
        };
        let CheckedClientExpression::Resource { operation } = expression.as_ref() else {
            panic!("named resource client must retain its resource operation");
        };
        assert_eq!(operation.target(), CheckedFunctionId::Existing(target_id));
        assert_eq!(operation.standard_result_type(), None);
        assert_eq!(
            operation.result_type(),
            SemanticType::Named(CheckedTypeId::Existing(action_type_id))
        );
        operation.call_site()
    };

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    let caller = prepared
        .candidate()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["resource_fixture", "call"])
        .unwrap();
    let revision = prepared
        .new_function_revisions()
        .iter()
        .find(|revision| revision.function() == caller.id())
        .unwrap();
    assert_eq!(revision.artifact().version(), CLIENT_PLAN_RESOURCE_VERSION);
    let plan = ResourceClientPlan::decode(revision.artifact().payload()).unwrap();
    let ClientExpressionNode::Await { expression } = plan.expression() else {
        panic!("prepared named resource plan must await the resource");
    };
    let ClientExpressionNode::Resource { operation } = expression.as_ref() else {
        panic!("prepared named resource plan must contain a resource operation");
    };
    assert_eq!(
        operation.kind(),
        orna_artifact::client_plan::ResourceKind::Scalar
    );
    assert_eq!(operation.target(), target_id);
    assert_eq!(operation.target_revision(), prepared.candidate_pair());
    assert_eq!(operation.result_type(), action_type_id);
    assert_eq!(operation.call_site(), checked_call_site);
    assert_ne!(operation.call_site().to_bytes(), [0; 16]);
    let caller_parameter = |name: &str| {
        caller
            .parameters()
            .iter()
            .find(|parameter| parameter.name() == name)
            .unwrap()
            .id()
    };
    let mut expected_arguments = vec![
        (
            target_second_parameter_id,
            ClientExpressionNode::ParameterRead {
                parameter: caller_parameter("p_second"),
            },
        ),
        (
            target_first_parameter_id,
            ClientExpressionNode::ParameterRead {
                parameter: caller_parameter("p_first"),
            },
        ),
    ];
    expected_arguments.sort_by_key(|(parameter, _)| *parameter);
    assert_eq!(operation.arguments(), expected_arguments.as_slice());
}

#[test]
fn standard_stream_resource_preparation_materialises_durable_operation_artifact() {
    let verified = resource_standard();
    let text_type_id = verified
        .catalogue()
        .value_type_by_name(&semantic_name(&["std", "types", "character_large_object"]))
        .unwrap()
        .id();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_standard_application_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let bundle = SourceBundle::new([SourceUnit::new(
        "fixtures/stream_resource_dogfood.orna",
        include_str!("../../../orna-server/tests/fixtures/stream_resource_dogfood.orna"),
    )])
    .unwrap();
    let report = check_standard_application(&bundle, &context);
    assert!(
        report.diagnostics().is_empty(),
        "accepted stream resource fixture did not check: {:?}",
        report.diagnostics()
    );

    let checked_call_site = {
        let checked = report.preparation_view().unwrap().checked();
        let client = checked
            .client_functions()
            .iter()
            .find(|function| function.name().parts() == ["stream_fixture", "read"])
            .unwrap();
        let CheckedClientFunctionBody::Expression { expression } = client.body() else {
            panic!("stream resource client must use an expression body");
        };
        let CheckedClientExpression::Await { expression, .. } = expression else {
            panic!("stream resource client must await its resource");
        };
        let CheckedClientExpression::Resource { operation } = expression.as_ref() else {
            panic!("stream resource client must retain its resource operation");
        };
        operation.call_site()
    };

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    let target = prepared
        .candidate()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["stream_fixture", "events"])
        .unwrap();
    let target_revision = prepared
        .new_function_revisions()
        .iter()
        .find(|revision| revision.function() == target.id())
        .unwrap();
    assert_eq!(target.current_revision(), target_revision.id());
    assert_eq!(
        target_revision.artifact().kind(),
        ExecutableArtifactKind::Server
    );
    assert_eq!(target_revision.artifact().format(), SERVER_PLAN_FORMAT);
    assert_eq!(target_revision.artifact().version(), SERVER_PLAN_VERSION);
    assert_eq!(
        target.return_type(),
        &FunctionReturn::Stream(ResolvedType::Value(text_type_id))
    );
    let target_plan = ServerPlan::decode(target_revision.artifact().payload()).unwrap();
    let probe = prepared
        .candidate()
        .object_type_by_name(&semantic_name(&["stream_fixture", "probe"]))
        .unwrap();
    assert_eq!(target_plan.scan.object_type, probe.id());
    assert_eq!(target_plan.projections.len(), 1);
    assert_eq!(
        target_plan.projections[0].value_type.resolved_type,
        ResolvedType::scalar(StandardScalar::CharacterLargeObject)
    );
    let ExpressionKind::FieldPath { ref steps, .. } = target_plan.projections[0].kind else {
        panic!("stream SERVER plan projection must be a field path");
    };
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].owner, probe.id());
    assert_eq!(steps[0].field, probe.field_by_name("marker").unwrap().id());

    let client = prepared
        .candidate()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["stream_fixture", "read"])
        .unwrap();
    let revision = prepared
        .new_function_revisions()
        .iter()
        .find(|revision| revision.function() == client.id())
        .unwrap();
    assert_eq!(revision.artifact().version(), CLIENT_PLAN_RESOURCE_VERSION);
    let plan = ResourceClientPlan::decode(revision.artifact().payload()).unwrap();
    let ClientExpressionNode::Await { expression } = plan.expression() else {
        panic!("prepared stream resource plan must keep AWAIT at the return expression");
    };
    let ClientExpressionNode::Resource {
        operation: artifact,
    } = expression.as_ref()
    else {
        panic!("prepared stream resource plan must contain a resource operation under AWAIT");
    };
    assert_eq!(
        artifact.kind(),
        orna_artifact::client_plan::ResourceKind::Stream
    );
    assert_eq!(artifact.target(), target.id());
    assert_eq!(artifact.target_revision(), prepared.candidate_pair());
    assert_eq!(artifact.result_type(), text_type_id);
    assert_eq!(artifact.call_site(), checked_call_site);
    assert_ne!(artifact.call_site().to_bytes(), [0; 16]);
    assert!(artifact.arguments().is_empty());
}

#[test]
fn procedural_stream_resource_preparation_preserves_local_and_operation_identity() {
    let verified = resource_standard();
    let text_type_id = verified
        .catalogue()
        .value_type_by_name(&semantic_name(&["std", "types", "character_large_object"]))
        .unwrap()
        .id();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_standard_application_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let source = r#"CREATE SCHEMA stream_fixture;

CREATE TYPE stream_fixture.probe AS OBJECT (
    marker TEXT NOT NULL
);

CREATE SERVER FUNCTION stream_fixture.events()
RETURNS STREAM<TEXT>
SECURITY INVOKER
TRANSACTION READ ONLY
VOLATILITY STABLE
AS
    SELECT probe.marker FROM stream_fixture.probe probe;

CREATE CLIENT FUNCTION stream_fixture.read_local() RETURNS STREAM<TEXT> IS
    LET events std.data.StreamResource<TEXT> := std.data.stream_resource(
        target => stream_fixture.events,
        arguments => std.call.args()
    );
BEGIN
    RETURN AWAIT events;
END;"#;
    let bundle = SourceBundle::new([SourceUnit::new(
        "fixtures/stream_resource_procedural.orna",
        source,
    )])
    .unwrap();
    let report = check_standard_application(&bundle, &context);
    assert!(
        report.diagnostics().is_empty(),
        "procedural stream resource fixture did not check: {:?}",
        report.diagnostics()
    );

    let checked_call_site = {
        let checked = report.preparation_view().unwrap().checked();
        let client = checked
            .client_functions()
            .iter()
            .find(|function| function.name().parts() == ["stream_fixture", "read_local"])
            .unwrap();
        let CheckedClientFunctionBody::Procedural {
            locals,
            statements,
            return_expression,
        } = client.body()
        else {
            panic!("procedural stream resource client must use its block body");
        };
        assert_eq!(locals.len(), 1);
        assert_eq!(statements.len(), 1);
        let CheckedClientStatement::Let { expression, .. } = &statements[0] else {
            panic!("procedural stream resource local must be initialized by LET");
        };
        let CheckedClientExpression::Resource { operation } = expression else {
            panic!("procedural stream resource client must retain its constructor");
        };
        assert_eq!(
            operation.kind(),
            orna_artifact::client_plan::ResourceKind::Stream
        );
        let CheckedClientExpression::Await { expression, .. } = return_expression else {
            panic!("procedural stream resource client must use its local");
        };
        assert!(matches!(
            expression.as_ref(),
            CheckedClientExpression::LocalRead { local: 0, .. }
        ));
        operation.call_site()
    };

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    let target = prepared
        .candidate()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["stream_fixture", "events"])
        .unwrap();
    let client = prepared
        .candidate()
        .functions()
        .iter()
        .find(|function| function.name().parts() == ["stream_fixture", "read_local"])
        .unwrap();
    let revision = prepared
        .new_function_revisions()
        .iter()
        .find(|revision| revision.function() == client.id())
        .unwrap();
    assert_eq!(
        revision.artifact().version(),
        CLIENT_PLAN_PROCEDURAL_VERSION
    );
    let plan = ProceduralClientPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(plan.locals().len(), 1);
    let local_decl = &plan.locals()[0];
    assert_eq!(
        local_decl.local_id(),
        durable_client_local_id(client.id(), 0)
    );
    assert_eq!(local_decl.type_id(), text_type_id);
    assert_eq!(
        local_decl.kind(),
        ClientLocalKind::Resource(orna_artifact::client_plan::ResourceKind::Stream)
    );
    assert_eq!(plan.statements().len(), 1);
    assert_eq!(plan.statements()[0].local(), local_decl.local_id());
    let ClientExpressionNode::Resource { operation } = plan.statements()[0].expression() else {
        panic!("procedural plan LET must contain a stream resource operation");
    };
    assert_eq!(
        operation.kind(),
        orna_artifact::client_plan::ResourceKind::Stream
    );
    assert_eq!(operation.target(), target.id());
    assert_eq!(operation.target_revision(), prepared.candidate_pair());
    assert_eq!(operation.call_site(), checked_call_site);
    assert_eq!(operation.result_type(), text_type_id);
    assert!(operation.arguments().is_empty());
    let ClientExpressionNode::Await { expression } = plan.return_expression() else {
        panic!("procedural plan return must await the resource local");
    };
    let ClientExpressionNode::LocalRead { local } = expression.as_ref() else {
        panic!("procedural plan return AWAIT must read the resource local");
    };
    assert_eq!(*local, local_decl.local_id());
}
#[test]
fn rejects_legacy_client_state_plan_without_standard_type_identity() {
    let active = empty_active();
    let source = "CREATE SCHEMA examples; \
            CREATE CLIENT FUNCTION examples.state() RETURNS BOOLEAN IS \
            STATE flag BOOLEAN DEFAULT TRUE; \
            BEGIN RETURN TRUE; END;";
    let report = checked_report(source, active.catalogue());
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );

    let result = prepare(&report, active.pair(), &active);
    assert!(
        matches!(
            result,
            Err(PrepareError::InvalidCheckedBundle { reason })
                if reason == "checked CLIENT state declarations require standard-backed preparation"
        ),
        "result: {result:?}"
    );
}

#[test]
fn standard_client_state_plan_uses_resolved_slot_type_and_default() {
    let verified = invocation_carrier_standard();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_standard_application_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let source = "CREATE SCHEMA examples; \
            CREATE CLIENT FUNCTION examples.ready() RETURNS BOOLEAN IS \
            STATE flag BOOLEAN DEFAULT TRUE; \
            STATE session_flag BOOLEAN SCOPE SESSION DEFAULT NULL; \
            STATE user_flag BOOLEAN SCOPE USER; \
            BEGIN RETURN TRUE; END;";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );

    let prepared = prepare_standard_application(&report, active.pair(), &active).unwrap();
    let revision = &prepared.new_function_revisions()[0];
    assert_eq!(revision.artifact().version(), CLIENT_PLAN_STATE_VERSION);
    let plan = StateClientPlan::decode(revision.artifact().payload()).unwrap();
    assert_eq!(
        plan.expression(),
        &ClientExpressionNode::Boolean { value: true }
    );
    assert_eq!(plan.slots().len(), 3);
    let function_id = prepared.candidate().functions()[0].id();
    let flag = &plan.slots()[0];
    assert_eq!(
        flag.state_slot_id(),
        durable_state_slot_id(function_id, "flag")
    );
    assert_eq!(flag.type_id(), TypeId::from_bytes([3; 16]));
    assert_eq!(flag.scope(), StateScope::Local);
    assert_eq!(
        flag.default(),
        &StateDefault::Expression(ClientExpressionNode::Boolean { value: true })
    );
    let session_flag = &plan.slots()[1];
    assert_eq!(
        session_flag.state_slot_id(),
        durable_state_slot_id(function_id, "session_flag")
    );
    assert_eq!(session_flag.type_id(), TypeId::from_bytes([3; 16]));
    assert_eq!(session_flag.scope(), StateScope::Session);
    assert_eq!(session_flag.default(), &StateDefault::Null);
    let user_flag = &plan.slots()[2];
    assert_eq!(
        user_flag.state_slot_id(),
        durable_state_slot_id(function_id, "user_flag")
    );
    assert_eq!(user_flag.type_id(), TypeId::from_bytes([3; 16]));
    assert_eq!(user_flag.scope(), StateScope::User);
    assert_eq!(user_flag.default(), &StateDefault::Unset);
}

#[test]
fn standard_client_state_declaration_evidence_rejects_tampered_owner_or_ordinal() {
    let verified = invocation_carrier_standard();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_standard_application_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let source = "CREATE SCHEMA examples; \
            CREATE CLIENT FUNCTION examples.ready() RETURNS BOOLEAN IS \
            STATE flag BOOLEAN DEFAULT TRUE; \
            BEGIN RETURN TRUE; END;";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let state_index = report
        .checked_bundle()
        .unwrap()
        .uses()
        .iter()
        .position(|type_use| matches!(type_use.kind(), crate::CheckedTypeUseKind::State { .. }))
        .unwrap();
    let state_kind = report.checked_bundle().unwrap().uses()[state_index].kind();
    let crate::CheckedTypeUseKind::State { owner, ordinal } = state_kind else {
        unreachable!();
    };

    for tampered_kind in [
        crate::CheckedTypeUseKind::State {
            owner: crate::CheckedFunctionId::Existing(FunctionId::from_bytes([0xf1; 16])),
            ordinal,
        },
        crate::CheckedTypeUseKind::State {
            owner,
            ordinal: ordinal + 1,
        },
    ] {
        let mut tampered = report.clone();
        assert!(tampered.replace_type_use_kind_for_test(state_index, tampered_kind));
        let error = prepare_standard_application(&tampered, active.pair(), &active)
            .expect_err("tampered state declaration evidence must fail closed");
        assert!(matches!(
            error,
            PrepareStandardApplicationError::DeclarationTypeEvidenceMismatch {
                kind: crate::CheckedTypeUseKind::State { .. },
            }
        ));
    }
}

#[test]
fn checked_client_capability_maps_to_the_artifact_requirement_carrier() {
    let read = crate::CheckedClientCapability::new(
        "std.fs.read",
        crate::CheckedClientCapabilityArgument::Text("/home/bob".to_owned()),
    );
    let requirement = client_capability_requirement(&read);
    assert_eq!(requirement.name(), "std.fs.read");
    assert_eq!(
        requirement.argument(),
        &CapabilityArgumentSource::Text("/home/bob".to_owned())
    );

    let secret = crate::CheckedClientCapability::new(
        "std.secret.use",
        crate::CheckedClientCapabilityArgument::Parameter("p_secret".to_owned()),
    );
    let requirement = client_capability_requirement(&secret);
    assert_eq!(requirement.name(), "std.secret.use");
    assert_eq!(
        requirement.argument(),
        &CapabilityArgumentSource::Parameter("p_secret".to_owned())
    );
}

#[test]
fn capability_client_plan_round_trips_the_emitted_version_five_envelope() {
    let requirements = vec![
        CapabilityRequirement::new(
            "std.fs.read",
            CapabilityArgumentSource::Text("/home/bob".to_owned()),
        ),
        CapabilityRequirement::new(
            "std.secret.use",
            CapabilityArgumentSource::Parameter("p_secret".to_owned()),
        ),
    ];
    let plan = CapabilityClientPlan::new(
        InnerClientPlan::Expression(ExpressionClientPlan::new(ClientExpressionNode::String {
            value: "ready".to_owned(),
        })),
        requirements,
    );
    assert_eq!(plan.format_version(), CLIENT_PLAN_CAPABILITY_VERSION);
    assert_eq!(plan.inner_plan_version(), CLIENT_PLAN_EXPRESSION_VERSION);

    let bytes = plan.encode().unwrap();
    assert_eq!(&bytes[8..12], &CLIENT_PLAN_CAPABILITY_VERSION.to_be_bytes());
    let decoded = CapabilityClientPlan::decode(&bytes).unwrap();
    assert_eq!(decoded.format_version(), CLIENT_PLAN_CAPABILITY_VERSION);
    assert_eq!(decoded.inner_plan_version(), CLIENT_PLAN_EXPRESSION_VERSION);
    let InnerClientPlan::Expression(inner) = decoded.inner_plan() else {
        panic!("inner plan must round-trip as an expression plan");
    };
    assert_eq!(
        inner.expression(),
        &ClientExpressionNode::String {
            value: "ready".to_owned()
        }
    );
    assert_eq!(decoded.requirements().len(), 2);
    assert_eq!(decoded.requirements()[0].name(), "std.fs.read");
    assert_eq!(
        decoded.requirements()[0].argument(),
        &CapabilityArgumentSource::Text("/home/bob".to_owned())
    );
    assert_eq!(decoded.requirements()[1].name(), "std.secret.use");
    assert_eq!(
        decoded.requirements()[1].argument(),
        &CapabilityArgumentSource::Parameter("p_secret".to_owned())
    );
}

fn checked_report(source: &str, base: &CatalogueSnapshot) -> CheckReport {
    let bundle = SourceBundle::new([SourceUnit::new("tasks.orna", source)]).unwrap();
    check(&bundle, base)
}
#[test]
fn prepares_generic_client_expression_without_standard_catalogue() {
    let active = empty_active();
    let source = "CREATE SCHEMA examples; CREATE CLIENT FUNCTION examples.add(p_value INTEGER) RETURNS INTEGER RETURN p_value + 1;";
    let report = checked_report(source, active.catalogue());
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let prepared = prepare(&report, active.pair(), &active).unwrap();
    let function = prepared
        .candidate()
        .function_by_name(&semantic_name(&["examples", "add"]))
        .unwrap();
    assert_eq!(function.domain(), FunctionDomain::Client);
    assert_eq!(
        function.return_type(),
        &FunctionReturn::Single(ResolvedType::Scalar(StandardScalar::Integer)),
    );
    assert_eq!(prepared.new_function_revisions().len(), 1);
}

type DistinctFixture = (
    crate::relational::DistinctQueryIr<TypeId, FieldId>,
    FunctionDefinition,
    Vec<ObjectTypeDefinition>,
    Vec<(DefinitionReferenceKind, DefinitionReferenceTarget)>,
);

type VersionOneFixture = (
    crate::relational::RelationalQueryIr<TypeId, FieldId>,
    FunctionDefinition,
    Vec<ObjectTypeDefinition>,
    Vec<(DefinitionReferenceKind, DefinitionReferenceTarget)>,
);

fn mapped_version_one_fixture() -> VersionOneFixture {
    mapped_version_one_fixture_for(SOURCE)
}

fn mapped_version_one_fixture_for(source: &str) -> VersionOneFixture {
    let active = empty_active();
    let report = checked_report(source, active.catalogue());
    let prepared = prepare(&report, active.pair(), &active).unwrap();
    let checked = report.checked_bundle().unwrap();
    let mut type_ids = std::collections::HashMap::new();
    let mut field_ids = std::collections::HashMap::new();
    for checked_object in checked.object_types() {
        let candidate = prepared
            .candidate()
            .object_type_by_name(checked_object.name())
            .unwrap();
        type_ids.insert(checked_object.id(), candidate.id());
        for checked_field in checked_object.fields() {
            field_ids.insert(
                checked_field.id(),
                candidate.field_by_name(checked_field.name()).unwrap().id(),
            );
        }
    }
    let plan = checked.server_functions()[0]
        .query_plan()
        .unwrap()
        .try_map_identities(
            |id| {
                type_ids
                    .get(&id)
                    .copied()
                    .ok_or(PrepareError::InvalidCheckedBundle {
                        reason: "type mapping is absent",
                    })
            },
            |id| {
                field_ids
                    .get(&id)
                    .copied()
                    .ok_or(PrepareError::InvalidCheckedBundle {
                        reason: "field mapping is absent",
                    })
            },
        )
        .unwrap();
    let function = prepared.candidate().functions()[0].clone();
    let object_types = prepared.candidate().object_types().to_vec();
    let references = version_one_query_reference_sequence(&plan, &function);
    (plan, function, object_types, references)
}

fn mapped_distinct_fixture() -> DistinctFixture {
    mapped_distinct_fixture_for(DISTINCT_SOURCE)
}

fn mapped_distinct_fixture_for(source: &str) -> DistinctFixture {
    let active = empty_active();
    let report = checked_report(source, active.catalogue());
    let prepared = prepare(&report, active.pair(), &active).unwrap();
    let checked = report.checked_bundle().unwrap();
    let mut type_ids = std::collections::HashMap::new();
    let mut field_ids = std::collections::HashMap::new();
    for checked_object in checked.object_types() {
        let candidate = prepared
            .candidate()
            .object_type_by_name(checked_object.name())
            .unwrap();
        type_ids.insert(checked_object.id(), candidate.id());
        for checked_field in checked_object.fields() {
            field_ids.insert(
                checked_field.id(),
                candidate.field_by_name(checked_field.name()).unwrap().id(),
            );
        }
    }
    let plan = checked.server_functions()[0]
        .distinct_query_plan()
        .unwrap()
        .try_map_identities(
            |id| {
                type_ids
                    .get(&id)
                    .copied()
                    .ok_or(PrepareError::InvalidCheckedBundle {
                        reason: "type mapping is absent",
                    })
            },
            |id| {
                field_ids
                    .get(&id)
                    .copied()
                    .ok_or(PrepareError::InvalidCheckedBundle {
                        reason: "field mapping is absent",
                    })
            },
        )
        .unwrap();
    let function = prepared.candidate().functions()[0].clone();
    let object_types = prepared.candidate().object_types().to_vec();
    let references = distinct_query_reference_sequence(&plan, &function);
    (plan, function, object_types, references)
}

fn object_types_with_distinct_completed_type(
    object_types: &[ObjectTypeDefinition],
    resolved_type: ResolvedType,
) -> Vec<ObjectTypeDefinition> {
    object_types
        .iter()
        .map(|object_type| {
            if object_type.name() != &semantic_name(&["tasks", "task"]) {
                return object_type.clone();
            }
            ObjectTypeDefinition::new(
                object_type.id(),
                object_type.name().clone(),
                object_type
                    .fields()
                    .iter()
                    .map(|field| {
                        if field.name() == "completed" {
                            FieldDefinition::new(
                                field.id(),
                                field.name(),
                                field.ordinal(),
                                resolved_type,
                                field.nullable(),
                                field.unique(),
                                field.default_expression(),
                                field.on_delete(),
                            )
                        } else {
                            field.clone()
                        }
                    })
                    .collect(),
            )
        })
        .collect()
}

fn object_types_with_task_field(
    object_types: &[ObjectTypeDefinition],
    field_name: &str,
    resolved_type: ResolvedType,
    nullable: bool,
) -> Vec<ObjectTypeDefinition> {
    object_types
        .iter()
        .map(|object_type| {
            if object_type.name() != &semantic_name(&["tasks", "task"]) {
                return object_type.clone();
            }
            ObjectTypeDefinition::new(
                object_type.id(),
                object_type.name().clone(),
                object_type
                    .fields()
                    .iter()
                    .map(|field| {
                        if field.name() != field_name {
                            return field.clone();
                        }
                        FieldDefinition::new(
                            field.id(),
                            field.name(),
                            field.ordinal(),
                            resolved_type,
                            nullable,
                            field.unique(),
                            field.default_expression(),
                            field.on_delete(),
                        )
                    })
                    .collect(),
            )
        })
        .collect()
}

fn distinct_function_with_completed_type(
    function: &FunctionDefinition,
    resolved_type: ResolvedType,
) -> FunctionDefinition {
    let FunctionReturn::Rows(columns) = function.return_type() else {
        panic!("DISTINCT fixture function must return ROWS");
    };
    let mut columns = columns.to_vec();
    let completed = &columns[2];
    columns[2] =
        FunctionReturnColumnDefinition::new(completed.name(), completed.ordinal(), resolved_type);
    FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        function.domain(),
        function.parameters().to_vec(),
        FunctionReturn::Rows(columns),
        function.current_revision(),
        function.security(),
        function.transaction(),
        function.volatility(),
    )
}

fn mapped_distinct_plan(
    plan: &crate::relational::DistinctQueryIr<TypeId, FieldId>,
    map_type: impl FnMut(TypeId) -> Result<TypeId, PrepareError>,
    map_field: impl FnMut(FieldId) -> Result<FieldId, PrepareError>,
) -> Result<crate::relational::DistinctQueryIr<TypeId, FieldId>, PrepareError> {
    plan.try_map_identities(map_type, map_field)
}

fn assert_preparation_reason<T>(result: Result<T, PrepareError>, reason: &'static str) {
    assert!(matches!(
        result,
        Err(PrepareError::InvalidCheckedBundle { reason: actual }) if actual == reason
    ));
}

fn empty_active() -> ActiveDatabaseRevision {
    let source_bundle = SourceBundleId::new();
    let source_revision = SourceRevisionId::new();
    let bundle_hash = source_bundle_digest(&[]).unwrap();
    let source = StoredSourceRevision::new(
        source_bundle,
        source_revision,
        None,
        Vec::new(),
        bundle_hash,
        source_revision_record_digest(source_bundle, None, bundle_hash).unwrap(),
    )
    .unwrap();
    let catalogue =
        CatalogueSnapshot::new(CatalogueRevisionId::new(), Vec::new(), Vec::new()).unwrap();
    let pair = RevisionPair::new(source.id(), catalogue.revision());
    let catalogue_hash = catalogue_digest(&catalogue, &[], &[], &[], &[]).unwrap();
    ActiveDatabaseRevision::new(
        pair,
        source,
        catalogue,
        catalogue_hash,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .unwrap()
}

fn shared_expression_active() -> ActiveDatabaseRevision {
    let schema = SchemaDefinition::new(SchemaId::new(), semantic_name(&["demo"]));
    let object_type_id = TypeId::new();
    let first_field = FieldId::new();
    let second_field = FieldId::new();
    let expression_id = ExpressionId::new();
    let catalogue = CatalogueSnapshot::new(
        CatalogueRevisionId::new(),
        vec![schema.clone()],
        vec![ObjectTypeDefinition::new(
            object_type_id,
            semantic_name(&["demo", "item"]),
            vec![
                FieldDefinition::new(
                    first_field,
                    "first",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                    true,
                    false,
                    Some(expression_id),
                    None,
                ),
                FieldDefinition::new(
                    second_field,
                    "second",
                    1,
                    ResolvedType::scalar(StandardScalar::Integer),
                    true,
                    false,
                    Some(expression_id),
                    None,
                ),
            ],
        )],
    )
    .unwrap();

    let source_bundle = SourceBundleId::new();
    let source_revision = SourceRevisionId::new();
    let source_unit = SourceUnitId::new();
    let content_hash = source_unit_content_digest(SHARED_EXPRESSION_SOURCE).unwrap();
    let unit = StoredSourceUnit::new(
        source_unit,
        0,
        "tasks.orna",
        SHARED_EXPRESSION_SOURCE,
        content_hash,
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&unit)).unwrap();
    let source = StoredSourceRevision::new(
        source_bundle,
        source_revision,
        None,
        vec![unit],
        bundle_hash,
        source_revision_record_digest(source_bundle, None, bundle_hash).unwrap(),
    )
    .unwrap();
    let origin = SourceOrigin::new(source_unit, 0, SHARED_EXPRESSION_SOURCE.len() as u32).unwrap();
    let origins = vec![
        DefinitionOrigin::new(DefinitionIdentity::Schema(schema.id()), origin),
        DefinitionOrigin::new(DefinitionIdentity::ObjectType(object_type_id), origin),
        DefinitionOrigin::new(
            DefinitionIdentity::Field {
                owner: object_type_id,
                field: first_field,
            },
            origin,
        ),
        DefinitionOrigin::new(
            DefinitionIdentity::Field {
                owner: object_type_id,
                field: second_field,
            },
            origin,
        ),
        DefinitionOrigin::new(DefinitionIdentity::Expression(expression_id), origin),
    ];
    let payload = ConstantExpression::Integer(1).encode().unwrap();
    let artifact = ExpressionArtifact::new(
        expression_id,
        CONSTANT_FORMAT,
        CONSTANT_VERSION,
        payload.clone(),
        artifact_payload_digest(&payload).unwrap(),
    )
    .unwrap();
    let expressions = vec![artifact];
    let pair = RevisionPair::new(source.id(), catalogue.revision());
    let catalogue_hash = catalogue_digest(&catalogue, &[], &expressions, &origins, &[]).unwrap();
    ActiveDatabaseRevision::new(
        pair,
        source,
        catalogue,
        catalogue_hash,
        expressions,
        Vec::new(),
        origins,
        Vec::new(),
    )
    .unwrap()
}

fn activate(
    prepared: &DeployableRevision,
    current: Vec<FunctionRevisionRecord>,
    history: Vec<FunctionRevisionRecord>,
) -> ActiveDatabaseRevision {
    ActiveDatabaseRevision::new_with_history(
        prepared.candidate_pair(),
        prepared.source().clone(),
        prepared.candidate().clone(),
        prepared.catalogue_hash(),
        prepared.expressions().to_vec(),
        current,
        history,
        prepared.origins().to_vec(),
        prepared.references().to_vec(),
    )
    .unwrap()
}

fn semantic_name(parts: &[&str]) -> orna_core::catalogue::QualifiedSemanticName {
    orna_core::catalogue::QualifiedSemanticName::new(parts.iter().copied()).unwrap()
}

fn expression<'a>(
    expressions: &'a [ExpressionArtifact],
    field: &FieldDefinition,
) -> &'a ExpressionArtifact {
    let id = field.default_expression().unwrap();
    expressions
        .iter()
        .find(|expression| expression.id() == id)
        .unwrap()
}

const fn digest(byte: u8) -> Sha256Digest {
    Sha256Digest::from_bytes([byte; 32])
}
