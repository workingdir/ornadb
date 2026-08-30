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
    StandardApplicationCheckContext, StandardSourceIdentitySeed, check, check_standard_application,
    check_standard_library_source,
    mutation::{
        MutationAssignment, MutationExpression, MutationExpressionKind,
        MutationRecordFieldExpression, MutationRecordFieldExpressionKind, MutationValueType,
    },
    prepare_standard_source,
};
mod client;
mod mutation;
mod query;

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
fn prepares_source_authored_math_with_seeded_identities() {
    let verified = crate::tests::verified_canonical_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_standard_application_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let source = include_str!("../../../../stdlib/std/math.orna");
    let bundle = SourceBundle::new([SourceUnit::new("std/math.orna", source)]).unwrap();
    let report = check_standard_application(&bundle, &context);
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let seed = StandardSourceIdentitySeed {
        catalogue_revision: CatalogueRevisionId::from_bytes([0x11; 16]),
        source_bundle: SourceBundleId::from_bytes([0x12; 16]),
        source_revision: SourceRevisionId::from_bytes([0x13; 16]),
        source_units: vec![SourceUnitId::from_bytes([0x14; 16])],
        schema: SchemaId::from_bytes([0x10; 16]),
        functions: (0x15..=0x1a)
            .map(|value| FunctionId::from_bytes([value; 16]))
            .collect(),
        parameters: vec![
            vec![ParameterId::from_bytes([0x21; 16])],
            vec![ParameterId::from_bytes([0x22; 16])],
            vec![ParameterId::from_bytes([0x23; 16])],
            vec![
                ParameterId::from_bytes([0x24; 16]),
                ParameterId::from_bytes([0x25; 16]),
            ],
            vec![
                ParameterId::from_bytes([0x26; 16]),
                ParameterId::from_bytes([0x27; 16]),
            ],
            vec![
                ParameterId::from_bytes([0x28; 16]),
                ParameterId::from_bytes([0x29; 16]),
                ParameterId::from_bytes([0x2a; 16]),
            ],
        ],
        revisions: (0x31..=0x36)
            .map(|value| FunctionRevisionId::from_bytes([value; 16]))
            .collect(),
    };
    let prepared = prepare_standard_source(&report, active.pair(), &active, &seed).unwrap();
    assert_eq!(
        prepared
            .candidate()
            .schema_by_name(&semantic_name(&["std", "math"]))
            .unwrap()
            .id(),
        seed.schema
    );
    for (index, name) in ["increment", "decrement", "is_zero", "min", "max", "clamp"]
        .iter()
        .enumerate()
    {
        let function = prepared
            .candidate()
            .function_by_name(&semantic_name(&["std", "math", name]))
            .unwrap();
        assert_eq!(function.id(), seed.functions[index]);
        assert_eq!(
            function
                .parameters()
                .iter()
                .map(ParameterDefinition::id)
                .collect::<Vec<_>>(),
            seed.parameters[index]
        );
        assert_eq!(
            prepared
                .new_function_revisions()
                .iter()
                .find(|revision| revision.function() == function.id())
                .unwrap()
                .id(),
            seed.revisions[index]
        );
    }
    assert_eq!(
        prepared.new_function_revisions().len(),
        seed.revisions.len()
    );
    assert!(
        prepared
            .new_function_revisions()
            .iter()
            .all(|revision| !revision.artifact().payload().is_empty())
    );
}

#[test]
fn standard_source_revision_seed_must_match_checked_functions() {
    let verified = crate::tests::verified_canonical_standard_source_fixture();
    let standard = check_standard_library_source(&verified).unwrap();
    let active = empty_standard_application_active(&verified);
    let context = StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
    let bundle = SourceBundle::new([SourceUnit::new(
        "std/math.orna",
        include_str!("../../../../stdlib/std/math.orna"),
    )])
    .unwrap();
    let report = check_standard_application(&bundle, &context);
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let seed = StandardSourceIdentitySeed {
        catalogue_revision: CatalogueRevisionId::from_bytes([0x11; 16]),
        source_bundle: SourceBundleId::from_bytes([0x12; 16]),
        source_revision: SourceRevisionId::from_bytes([0x13; 16]),
        source_units: vec![SourceUnitId::from_bytes([0x14; 16])],
        schema: SchemaId::from_bytes([0x10; 16]),
        functions: Vec::new(),
        parameters: Vec::new(),
        revisions: Vec::new(),
    };
    let error = prepare_standard_source(&report, active.pair(), &active, &seed).unwrap_err();
    assert!(matches!(
        error,
        PrepareStandardApplicationError::Prepare {
            source: PrepareError::InvalidCheckedBundle { .. },
        }
    ));
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

fn checked_report(source: &str, base: &CatalogueSnapshot) -> CheckReport {
    let bundle = SourceBundle::new([SourceUnit::new("tasks.orna", source)]).unwrap();
    check(&bundle, base)
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
