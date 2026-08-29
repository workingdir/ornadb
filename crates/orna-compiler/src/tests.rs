//! Compiler integration and regression tests.
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

use orna_artifact::server_plan::{SelectBindValue, UniqueTextSelectedServerPlan};
use orna_core::{
    CatalogueRevisionId, ExpressionId, FunctionId, FunctionRevisionId, SchemaId, SourceBundleId,
    SourceRevisionId, SourceUnitId, StandardLibraryRevisionId, TypeId,
    canonical_hash::{
        artifact_payload_digest, catalogue_digest, catalogue_digest_with_context,
        function_semantic_digest, source_bundle_digest, source_revision_record_digest,
        source_unit_content_digest, verify_standard_library_snapshot,
    },
    catalogue::{
        CatalogueSnapshot, FunctionDefinition, FunctionDomain, FunctionReturn, FunctionSecurity,
        FunctionTransaction, FunctionVolatility, ObjectTypeDefinition, PreludeTypeName,
        QualifiedSemanticName, SchemaDefinition, TypeBinding, ValueTypeDefinition, ValueTypeKind,
        ValueTypeMutability, ValueTypePersistence,
    },
    revision::{
        ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
        CatalogueHashContext, CatalogueHashVersion, DefinitionIdentity, DefinitionOrigin,
        DefinitionReferenceKind, DefinitionReferenceTarget, DeployableRevision, ExecutableArtifact,
        ExecutableArtifactKind, ExpressionArtifact, FunctionRevisionRecord,
        FunctionSemanticHashVersion, RevisionPair, Sha256Digest, SourceOrigin,
        StandardLibraryDigestVersion, StandardLibrarySnapshot, StoredSourceRevision,
        StoredSourceUnit,
    },
    source::{SourceBundle, SourceUnit},
    types::{ResolvedType, StandardScalar},
};

use super::{
    CheckedFunctionId, CheckedParameterId, CheckedStandardLibrary, CheckedTypeId,
    CheckedTypeUseKind, CheckedValueTypeUse, DiagnosticCode,
    EMPTY_APPLICATION_CATALOGUE_REVISION_ID, PrepareError, PrepareStandardApplicationError,
    PrepareStandardUpgradeError, PreparedStandardUpgrade, StandardApplicationCheckContext,
    StandardApplicationCheckReport, StandardApplicationContextError, StandardLibraryCheckError,
    check, check_new_application, check_standard_application, check_standard_library_source,
    parse_bundle, prepare, prepare_checked_standard_upgrade, prepare_standard_application,
};
use crate::prepare::{
    CandidateAllocator, CandidateIdSource, ReservedStandardIds,
    prepare_checked_standard_upgrade_with_allocator, prepare_standard_application_with_allocator,
};
mod standard_application;
mod standard_upgrade;

const SUCCESS_STANDARD_DIGEST: [u8; 32] = [
    0x10, 0x61, 0xb8, 0x16, 0x88, 0x39, 0xaa, 0x50, 0x60, 0xbd, 0x4e, 0x5a, 0xef, 0x1e, 0xc8, 0x68,
    0x08, 0x22, 0x02, 0xb2, 0x96, 0x91, 0x42, 0x2a, 0xd9, 0x1a, 0x29, 0x64, 0x9c, 0x72, 0x0e, 0x83,
];

const NON_STD_SCHEMA_STANDARD_DIGEST: [u8; 32] = [
    0x2f, 0x79, 0x81, 0x75, 0x91, 0xcc, 0xdc, 0x83, 0x54, 0xea, 0xfc, 0x6c, 0x7a, 0x59, 0xb2, 0x4f,
    0x12, 0x36, 0x60, 0xae, 0x7f, 0x65, 0xc2, 0x76, 0x8c, 0x5b, 0x0d, 0x9a, 0xcf, 0x94, 0x35, 0x49,
];

const CANONICAL_STANDARD_SOURCE: &str = include_str!("../../../stdlib/std/types.orna");

static PREPARE_CATALOGUE_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static PREPARE_BUNDLE_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static PREPARE_REVISION_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static PREPARE_UNIT_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static PREPARE_FUNCTION_REVISION_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static PREPARE_SCHEMA_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static PREPARE_TYPE_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static PREPARE_ALLOCATION_LOCK: Mutex<()> = Mutex::new(());

fn retry_id(counter: &AtomicUsize, reserved: u8, safe: u8) -> u8 {
    if counter.fetch_add(1, Ordering::SeqCst) == 0 {
        reserved
    } else {
        safe
    }
}

fn retry_catalogue_id() -> CatalogueRevisionId {
    CatalogueRevisionId::from_bytes([retry_id(&PREPARE_CATALOGUE_ALLOCATIONS, 8, 0x81); 16])
}

fn retry_bundle_id() -> SourceBundleId {
    SourceBundleId::from_bytes([retry_id(&PREPARE_BUNDLE_ALLOCATIONS, 5, 0x82); 16])
}

fn retry_revision_id() -> SourceRevisionId {
    SourceRevisionId::from_bytes([retry_id(&PREPARE_REVISION_ALLOCATIONS, 6, 0x83); 16])
}

fn retry_unit_id() -> SourceUnitId {
    let allocation = PREPARE_UNIT_ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
    if allocation == 0 {
        return SourceUnitId::from_bytes(CANONICAL_RESERVED_ID);
    }
    let byte = if allocation == 1 {
        0x84
    } else {
        0x84 + u8::try_from(allocation - 1).unwrap()
    };
    SourceUnitId::from_bytes([byte; 16])
}

fn retry_schema_id() -> SchemaId {
    SchemaId::from_bytes([retry_id(&PREPARE_SCHEMA_ALLOCATIONS, 1, 0x85); 16])
}

fn retry_type_id() -> TypeId {
    TypeId::from_bytes([retry_id(&PREPARE_TYPE_ALLOCATIONS, 3, 0x86); 16])
}

fn next_function_revision_id() -> FunctionRevisionId {
    let allocation = PREPARE_FUNCTION_REVISION_ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
    FunctionRevisionId::from_bytes([0x90 + u8::try_from(allocation).unwrap(); 16])
}

fn retrying_standard_allocator(
    verified: &orna_core::revision::VerifiedStandardLibrarySnapshot,
) -> CandidateAllocator {
    for counter in [
        &PREPARE_CATALOGUE_ALLOCATIONS,
        &PREPARE_BUNDLE_ALLOCATIONS,
        &PREPARE_REVISION_ALLOCATIONS,
        &PREPARE_UNIT_ALLOCATIONS,
        &PREPARE_FUNCTION_REVISION_ALLOCATIONS,
        &PREPARE_SCHEMA_ALLOCATIONS,
        &PREPARE_TYPE_ALLOCATIONS,
    ] {
        counter.store(0, Ordering::SeqCst);
    }
    CandidateAllocator::with_source(
        ReservedStandardIds::from_snapshot(verified),
        CandidateIdSource {
            catalogue_revision: retry_catalogue_id,
            source_bundle: retry_bundle_id,
            source_revision: retry_revision_id,
            source_unit: retry_unit_id,
            schema: retry_schema_id,
            type_id: retry_type_id,
            function_revision: next_function_revision_id,
        },
    )
}

fn assert_no_standard_upgrade_allocations() {
    for counter in [
        &PREPARE_CATALOGUE_ALLOCATIONS,
        &PREPARE_BUNDLE_ALLOCATIONS,
        &PREPARE_REVISION_ALLOCATIONS,
        &PREPARE_UNIT_ALLOCATIONS,
        &PREPARE_FUNCTION_REVISION_ALLOCATIONS,
        &PREPARE_SCHEMA_ALLOCATIONS,
        &PREPARE_TYPE_ALLOCATIONS,
    ] {
        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "standard upgrade gate must reject before candidate allocation"
        );
    }
}

fn checked_type_use_kind_tag(kind: CheckedTypeUseKind) -> &'static str {
    match kind {
        CheckedTypeUseKind::Field { .. } => "field",
        CheckedTypeUseKind::Parameter { .. } => "parameter",
        CheckedTypeUseKind::State { .. } => "state",
        CheckedTypeUseKind::Return { .. } => "return",
        CheckedTypeUseKind::Expression { .. } => "expression",
        CheckedTypeUseKind::Result { .. } => "result",
    }
}

fn assert_declaration_evidence_mismatch(
    error: PrepareStandardApplicationError,
    expected_kind: CheckedTypeUseKind,
) {
    assert!(matches!(
        &error,
        PrepareStandardApplicationError::DeclarationTypeEvidenceMismatch { .. }
    ));
    if let PrepareStandardApplicationError::DeclarationTypeEvidenceMismatch { kind } = &error {
        assert_eq!(*kind, expected_kind);
    }
    assert_eq!(
        error.to_string(),
        format!(
            "the checked declaration type evidence does not match its {} type use",
            checked_type_use_kind_tag(expected_kind)
        )
    );
    assert!(std::error::Error::source(&error).is_none());
}

fn assert_body_evidence_mismatch(
    error: PrepareStandardApplicationError,
    expected_function: CheckedFunctionId,
) {
    assert!(matches!(
        &error,
        PrepareStandardApplicationError::BodyTypeEvidenceMismatch { .. }
    ));
    if let PrepareStandardApplicationError::BodyTypeEvidenceMismatch { function } = &error {
        assert_eq!(*function, expected_function);
    }
    assert_eq!(
        error.to_string(),
        format!("the checked body type evidence does not match function {expected_function}")
    );
    assert!(std::error::Error::source(&error).is_none());
}

fn assert_function_reference_evidence_mismatch(
    error: PrepareStandardApplicationError,
    expected_function: CheckedFunctionId,
) {
    assert!(matches!(
        &error,
        PrepareStandardApplicationError::FunctionTypeReferenceMismatch { .. }
    ));
    if let PrepareStandardApplicationError::FunctionTypeReferenceMismatch { function } = &error {
        assert_eq!(*function, expected_function);
    }
    assert_eq!(
        error.to_string(),
        format!("the checked function type references do not match function {expected_function}")
    );
    assert!(std::error::Error::source(&error).is_none());
}

fn assert_check_not_complete(error: PrepareStandardApplicationError, expected_count: usize) {
    assert!(matches!(
        &error,
        PrepareStandardApplicationError::CheckNotComplete { .. }
    ));
    if let PrepareStandardApplicationError::CheckNotComplete { diagnostic_count } = &error {
        assert_eq!(*diagnostic_count, expected_count);
    }
    assert_eq!(
        error.to_string(),
        format!("the standard application check has {expected_count} diagnostics")
    );
    assert!(std::error::Error::source(&error).is_none());
}

fn assert_expected_base_mismatch(
    error: PrepareStandardApplicationError,
    expected_expected: RevisionPair,
    expected_active: RevisionPair,
) {
    assert!(matches!(
        &error,
        PrepareStandardApplicationError::ExpectedBaseMismatch { .. }
    ));
    if let PrepareStandardApplicationError::ExpectedBaseMismatch { expected, active } = &error {
        assert_eq!(*expected, expected_expected);
        assert_eq!(*active, expected_active);
    }
    assert_eq!(
        error.to_string(),
        "the expected application base does not match the active revision"
    );
    assert!(std::error::Error::source(&error).is_none());
}

fn assert_checked_base_mismatch(
    error: PrepareStandardApplicationError,
    expected_checked: CatalogueRevisionId,
    expected_active: CatalogueRevisionId,
) {
    assert!(matches!(
        &error,
        PrepareStandardApplicationError::CheckedBaseMismatch { .. }
    ));
    if let PrepareStandardApplicationError::CheckedBaseMismatch { checked, active } = &error {
        assert_eq!(*checked, expected_checked);
        assert_eq!(*active, expected_active);
    }
    assert_eq!(
        error.to_string(),
        "the checked application base does not match the active revision"
    );
    assert!(std::error::Error::source(&error).is_none());
}

fn assert_standard_library_unavailable(error: PrepareStandardApplicationError) {
    assert!(matches!(
        &error,
        PrepareStandardApplicationError::StandardLibraryUnavailable
    ));
    assert_eq!(
        error.to_string(),
        "the active database has no standard library"
    );
    assert!(std::error::Error::source(&error).is_none());
}

fn assert_standard_catalogue_mismatch(
    error: PrepareStandardApplicationError,
    expected_checked: CatalogueRevisionId,
    expected_active: CatalogueRevisionId,
) {
    assert!(matches!(
        &error,
        PrepareStandardApplicationError::StandardCatalogueMismatch { .. }
    ));
    if let PrepareStandardApplicationError::StandardCatalogueMismatch { checked, active } = &error {
        assert_eq!(*checked, expected_checked);
        assert_eq!(*active, expected_active);
    }
    assert_eq!(
        error.to_string(),
        "the checked standard catalogue does not match the active standard catalogue"
    );
    assert!(std::error::Error::source(&error).is_none());
}

fn assert_standard_revision_mismatch(
    error: PrepareStandardApplicationError,
    expected_checked: StandardLibraryRevisionId,
    expected_active: StandardLibraryRevisionId,
) {
    assert!(matches!(
        &error,
        PrepareStandardApplicationError::StandardRevisionMismatch { .. }
    ));
    if let PrepareStandardApplicationError::StandardRevisionMismatch { checked, active } = &error {
        assert_eq!(*checked, expected_checked);
        assert_eq!(*active, expected_active);
    }
    assert_eq!(
        error.to_string(),
        "the checked standard library revision does not match the active standard library revision"
    );
    assert!(std::error::Error::source(&error).is_none());
}

fn assert_standard_digest_mismatch(
    error: PrepareStandardApplicationError,
    expected_checked: Sha256Digest,
    expected_active: Sha256Digest,
) {
    assert!(matches!(
        &error,
        PrepareStandardApplicationError::StandardDigestMismatch { .. }
    ));
    if let PrepareStandardApplicationError::StandardDigestMismatch { checked, active } = &error {
        assert_eq!(*checked, expected_checked);
        assert_eq!(*active, expected_active);
    }
    assert_eq!(
        error.to_string(),
        "the checked standard library digest does not match the active standard library digest"
    );
    assert!(std::error::Error::source(&error).is_none());
}

const CANONICAL_RESERVED_ID: [u8; 16] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
];
const CANONICAL_SCHEMA_IDS: [[u8; 16]; 2] = [
    [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x01,
    ],
    [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x02,
    ],
];
const CANONICAL_TYPE_IDS: [[u8; 16]; 14] = [
    [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x01,
    ],
    [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x02,
    ],
    [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x03,
    ],
    [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x04,
    ],
    [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x05,
    ],
    [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x06,
    ],
    [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x07,
    ],
    [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x08,
    ],
    [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x09,
    ],
    [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x0a,
    ],
    [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x0b,
    ],
    [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x0c,
    ],
    [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x0d,
    ],
    [
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x0e,
    ],
];
const CANONICAL_STANDARD_DIGEST: [u8; 32] = [
    0xbe, 0x61, 0x9c, 0xaa, 0xf6, 0xb2, 0x0b, 0xb7, 0xf8, 0xbc, 0x8d, 0xf9, 0x56, 0xd4, 0x89, 0xad,
    0xe4, 0x9b, 0xc8, 0xdf, 0xe0, 0x3c, 0xd6, 0xd9, 0x64, 0x70, 0x5b, 0x30, 0x23, 0x5b, 0x08, 0x1d,
];

#[derive(Clone, Copy)]
struct CanonicalValueTypeFact {
    name: &'static str,
    kind: ValueTypeKind,
    representation_contract: &'static str,
    persistence: ValueTypePersistence,
}

const CANONICAL_VALUE_TYPE_FACTS: [CanonicalValueTypeFact; 14] = [
    CanonicalValueTypeFact {
        name: "std.types.boolean",
        kind: ValueTypeKind::Primitive,
        representation_contract: "orna.kernel.value.boolean@1",
        persistence: ValueTypePersistence::Persistable,
    },
    CanonicalValueTypeFact {
        name: "std.types.integer",
        kind: ValueTypeKind::Primitive,
        representation_contract: "orna.kernel.value.integer@1",
        persistence: ValueTypePersistence::Persistable,
    },
    CanonicalValueTypeFact {
        name: "std.types.bigint",
        kind: ValueTypeKind::Primitive,
        representation_contract: "orna.kernel.value.bigint@1",
        persistence: ValueTypePersistence::Persistable,
    },
    CanonicalValueTypeFact {
        name: "std.types.float",
        kind: ValueTypeKind::Primitive,
        representation_contract: "orna.kernel.value.float@1",
        persistence: ValueTypePersistence::Persistable,
    },
    CanonicalValueTypeFact {
        name: "std.types.decimal",
        kind: ValueTypeKind::Primitive,
        representation_contract: "orna.kernel.value.decimal@1",
        persistence: ValueTypePersistence::Persistable,
    },
    CanonicalValueTypeFact {
        name: "std.types.character_large_object",
        kind: ValueTypeKind::Primitive,
        representation_contract: "orna.kernel.value.character-large-object@1",
        persistence: ValueTypePersistence::Persistable,
    },
    CanonicalValueTypeFact {
        name: "std.types.binary_large_object",
        kind: ValueTypeKind::Primitive,
        representation_contract: "orna.kernel.value.binary-large-object@1",
        persistence: ValueTypePersistence::Persistable,
    },
    CanonicalValueTypeFact {
        name: "std.types.uuid",
        kind: ValueTypeKind::Primitive,
        representation_contract: "orna.kernel.value.uuid@1",
        persistence: ValueTypePersistence::Persistable,
    },
    CanonicalValueTypeFact {
        name: "std.types.date",
        kind: ValueTypeKind::Primitive,
        representation_contract: "orna.kernel.value.date@1",
        persistence: ValueTypePersistence::Persistable,
    },
    CanonicalValueTypeFact {
        name: "std.types.time",
        kind: ValueTypeKind::Primitive,
        representation_contract: "orna.kernel.value.time@1",
        persistence: ValueTypePersistence::Persistable,
    },
    CanonicalValueTypeFact {
        name: "std.types.timestamp",
        kind: ValueTypeKind::Primitive,
        representation_contract: "orna.kernel.value.timestamp@1",
        persistence: ValueTypePersistence::Persistable,
    },
    CanonicalValueTypeFact {
        name: "std.types.duration",
        kind: ValueTypeKind::Primitive,
        representation_contract: "orna.kernel.value.duration@1",
        persistence: ValueTypePersistence::Persistable,
    },
    CanonicalValueTypeFact {
        name: "std.types.void",
        kind: ValueTypeKind::Primitive,
        representation_contract: "orna.kernel.value.void@1",
        persistence: ValueTypePersistence::Transient,
    },
    CanonicalValueTypeFact {
        name: "std.types.opaque_token",
        kind: ValueTypeKind::Opaque,
        representation_contract: "orna.std.value.opaque-token@1",
        persistence: ValueTypePersistence::Transient,
    },
];

#[derive(Clone, Copy)]
enum CanonicalBindingKind {
    Qualified,
    Prelude,
}

impl CanonicalBindingKind {
    const fn catalogue_kind(self) -> orna_core::catalogue::TypeBindingKind {
        match self {
            Self::Qualified => orna_core::catalogue::TypeBindingKind::Qualified,
            Self::Prelude => orna_core::catalogue::TypeBindingKind::Prelude,
        }
    }
}

#[derive(Clone, Copy)]
struct CanonicalBindingFact {
    kind: CanonicalBindingKind,
    name: &'static str,
    target_type_index: usize,
}

const CANONICAL_BINDING_FACTS: [CanonicalBindingFact; 31] = [
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Qualified,
        name: "std.boolean",
        target_type_index: 0,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Prelude,
        name: "boolean",
        target_type_index: 0,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Prelude,
        name: "bool",
        target_type_index: 0,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Qualified,
        name: "std.integer",
        target_type_index: 1,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Prelude,
        name: "integer",
        target_type_index: 1,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Prelude,
        name: "int",
        target_type_index: 1,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Qualified,
        name: "std.bigint",
        target_type_index: 2,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Prelude,
        name: "bigint",
        target_type_index: 2,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Qualified,
        name: "std.float",
        target_type_index: 3,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Prelude,
        name: "float",
        target_type_index: 3,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Qualified,
        name: "std.decimal",
        target_type_index: 4,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Prelude,
        name: "decimal",
        target_type_index: 4,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Qualified,
        name: "std.character_large_object",
        target_type_index: 5,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Prelude,
        name: "character large object",
        target_type_index: 5,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Prelude,
        name: "text",
        target_type_index: 5,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Qualified,
        name: "std.binary_large_object",
        target_type_index: 6,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Prelude,
        name: "binary large object",
        target_type_index: 6,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Prelude,
        name: "bytes",
        target_type_index: 6,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Qualified,
        name: "std.uuid",
        target_type_index: 7,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Prelude,
        name: "uuid",
        target_type_index: 7,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Qualified,
        name: "std.date",
        target_type_index: 8,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Prelude,
        name: "date",
        target_type_index: 8,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Qualified,
        name: "std.time",
        target_type_index: 9,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Prelude,
        name: "time",
        target_type_index: 9,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Qualified,
        name: "std.timestamp",
        target_type_index: 10,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Prelude,
        name: "timestamp",
        target_type_index: 10,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Qualified,
        name: "std.duration",
        target_type_index: 11,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Prelude,
        name: "duration",
        target_type_index: 11,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Qualified,
        name: "std.void",
        target_type_index: 12,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Prelude,
        name: "void",
        target_type_index: 12,
    },
    CanonicalBindingFact {
        kind: CanonicalBindingKind::Qualified,
        name: "std.opaque_token",
        target_type_index: 13,
    },
];

const CANONICAL_BINDING_IDS: [[u8; 16]; 31] = [
    [
        0x53, 0xf1, 0x37, 0x1e, 0xaf, 0xef, 0x9a, 0xe5, 0x34, 0x7f, 0x15, 0x5c, 0xf1, 0xdd, 0x4d,
        0x31,
    ],
    [
        0xfc, 0x31, 0x05, 0xaf, 0xaf, 0x25, 0x20, 0xd7, 0xc7, 0x7c, 0xdd, 0x6b, 0x0e, 0xf8, 0x15,
        0xaa,
    ],
    [
        0x7b, 0x20, 0xca, 0xb3, 0x61, 0x23, 0x35, 0x61, 0x03, 0xad, 0xab, 0x48, 0x61, 0x11, 0x0c,
        0xad,
    ],
    [
        0xf9, 0x2a, 0x68, 0x3c, 0xa4, 0x2b, 0x48, 0x2f, 0x77, 0x7a, 0x79, 0x86, 0xb2, 0xdf, 0x25,
        0x93,
    ],
    [
        0x19, 0x40, 0x9c, 0x7b, 0x37, 0x81, 0x68, 0xf8, 0x30, 0x0b, 0x44, 0x0c, 0xaf, 0x18, 0x57,
        0x78,
    ],
    [
        0x97, 0x0a, 0xa4, 0x1b, 0xb9, 0xb1, 0x99, 0xa3, 0xcb, 0xa3, 0x46, 0x8c, 0x9e, 0x7c, 0x58,
        0x89,
    ],
    [
        0x08, 0x52, 0xa1, 0xcb, 0xbe, 0x1c, 0x5b, 0x78, 0xb4, 0xfa, 0xd2, 0x9e, 0xed, 0x5b, 0x0d,
        0x1e,
    ],
    [
        0xa0, 0x50, 0x06, 0x28, 0xc9, 0x77, 0x06, 0xb2, 0xbd, 0x8f, 0x29, 0xf7, 0x8b, 0xaa, 0x5e,
        0x88,
    ],
    [
        0x30, 0x1f, 0x53, 0xba, 0x6e, 0xe1, 0xea, 0xd1, 0xe3, 0x18, 0x6b, 0x6b, 0x71, 0x9e, 0xfc,
        0xb5,
    ],
    [
        0x31, 0x03, 0xa7, 0xca, 0xfc, 0xc6, 0x3e, 0xd7, 0x2a, 0x10, 0x58, 0x00, 0x87, 0x97, 0xb5,
        0xe6,
    ],
    [
        0x28, 0x5c, 0x9a, 0x60, 0x1c, 0x08, 0x5b, 0xfa, 0xe9, 0x48, 0x5c, 0x9c, 0xb8, 0x6b, 0x45,
        0xf9,
    ],
    [
        0xdf, 0x8e, 0x7b, 0x74, 0x41, 0xca, 0xe1, 0xf8, 0xfd, 0x56, 0xd8, 0x83, 0xa3, 0x10, 0x6e,
        0xd5,
    ],
    [
        0x28, 0x67, 0x4f, 0xd2, 0x8e, 0x8a, 0x68, 0x08, 0x1e, 0x26, 0x3f, 0xb3, 0x1b, 0xc2, 0xd8,
        0x70,
    ],
    [
        0xf6, 0xd0, 0xd3, 0xb6, 0x31, 0x1b, 0x6b, 0xdc, 0xe6, 0x01, 0xd3, 0xcf, 0xc3, 0xa6, 0x89,
        0x1a,
    ],
    [
        0x72, 0x0f, 0xf6, 0x30, 0x3e, 0xf0, 0x01, 0x8c, 0x81, 0xd2, 0xa6, 0x73, 0x99, 0xf0, 0xdb,
        0xc2,
    ],
    [
        0xa9, 0x31, 0x64, 0x64, 0xe3, 0x52, 0xb5, 0x6a, 0x56, 0xa1, 0x4b, 0x38, 0x4c, 0x7d, 0x81,
        0x34,
    ],
    [
        0x15, 0x24, 0xb4, 0xca, 0x63, 0xbc, 0xe7, 0xf8, 0x9b, 0x24, 0xba, 0xf1, 0x8d, 0x33, 0xaf,
        0xbf,
    ],
    [
        0x84, 0xe0, 0x46, 0xbd, 0x87, 0xde, 0xc7, 0x0a, 0x1b, 0x73, 0x13, 0xae, 0x51, 0xb6, 0x9d,
        0xb7,
    ],
    [
        0x89, 0xea, 0x05, 0xd7, 0x14, 0xdc, 0x5d, 0x2f, 0x0a, 0x8e, 0x09, 0xf7, 0x5f, 0x31, 0x66,
        0x00,
    ],
    [
        0x73, 0xda, 0x8e, 0x2f, 0xac, 0xe9, 0x8a, 0x17, 0xa6, 0x63, 0xec, 0x97, 0xe6, 0x7c, 0x79,
        0x7f,
    ],
    [
        0xf9, 0x7c, 0x60, 0xa7, 0x50, 0x6b, 0x9e, 0x79, 0xa8, 0xa8, 0xd7, 0x84, 0xa1, 0x71, 0xf7,
        0xac,
    ],
    [
        0xf3, 0x2c, 0xab, 0x58, 0xdb, 0xdf, 0x3d, 0xc6, 0xfe, 0x7c, 0xb1, 0x74, 0x8e, 0x1f, 0x93,
        0x56,
    ],
    [
        0x15, 0x11, 0xd9, 0x2f, 0x12, 0xc3, 0x4c, 0x1b, 0x0c, 0x4c, 0x53, 0x26, 0xa8, 0xa0, 0x34,
        0x8d,
    ],
    [
        0x8b, 0xd8, 0x9d, 0x33, 0x32, 0x97, 0x8f, 0x32, 0xa7, 0xd0, 0xe1, 0xd6, 0x72, 0xd2, 0x33,
        0xd4,
    ],
    [
        0x47, 0xb0, 0x08, 0xa2, 0xdc, 0x0b, 0x20, 0xd1, 0x2b, 0x3e, 0x68, 0x9a, 0x30, 0xfc, 0xff,
        0x04,
    ],
    [
        0x84, 0x1f, 0xc4, 0xfb, 0x35, 0x7f, 0xf8, 0xc3, 0x10, 0x74, 0x4b, 0xfc, 0x97, 0x9c, 0x8a,
        0xa1,
    ],
    [
        0x36, 0x29, 0x37, 0xf6, 0x5e, 0x81, 0xf4, 0xa9, 0x45, 0x85, 0x47, 0xb4, 0xeb, 0x62, 0x14,
        0x9a,
    ],
    [
        0x6b, 0xdd, 0xb3, 0xa5, 0xf1, 0x4a, 0xc6, 0xf8, 0x42, 0x57, 0x35, 0xb8, 0x80, 0x2d, 0xdc,
        0x37,
    ],
    [
        0x82, 0xae, 0x45, 0x04, 0x07, 0xcf, 0xfa, 0xa6, 0x87, 0xe8, 0x1f, 0xa7, 0xdc, 0xbf, 0x94,
        0x0f,
    ],
    [
        0x56, 0xc5, 0x04, 0xe2, 0xf8, 0x07, 0xce, 0x24, 0xd3, 0x61, 0x11, 0xe6, 0x4a, 0x01, 0x73,
        0xfb,
    ],
    [
        0x4d, 0xab, 0x42, 0x83, 0x03, 0x1f, 0xcd, 0x81, 0xb5, 0x8d, 0x09, 0xd8, 0x87, 0x63, 0x46,
        0xae,
    ],
];

#[derive(Clone, Copy)]
enum CanonicalDeclarationIdentity {
    Schema(usize),
    ValueType(usize),
    TypeBinding(usize),
}

#[derive(Clone, Copy)]
struct CanonicalDeclarationFact {
    identity: CanonicalDeclarationIdentity,
    byte_start: u32,
    byte_end: u32,
}

const CANONICAL_DECLARATION_FACTS: [CanonicalDeclarationFact; 47] = [
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::Schema(0),
        byte_start: 0,
        byte_end: 18,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::Schema(1),
        byte_start: 19,
        byte_end: 43,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::ValueType(0),
        byte_start: 45,
        byte_end: 174,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(0),
        byte_start: 176,
        byte_end: 221,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(1),
        byte_start: 223,
        byte_end: 269,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(2),
        byte_start: 270,
        byte_end: 313,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::ValueType(1),
        byte_start: 315,
        byte_end: 444,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(3),
        byte_start: 446,
        byte_end: 491,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(4),
        byte_start: 493,
        byte_end: 539,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(5),
        byte_start: 540,
        byte_end: 582,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::ValueType(2),
        byte_start: 584,
        byte_end: 711,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(6),
        byte_start: 713,
        byte_end: 756,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(7),
        byte_start: 758,
        byte_end: 802,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::ValueType(3),
        byte_start: 804,
        byte_end: 929,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(8),
        byte_start: 931,
        byte_end: 972,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(9),
        byte_start: 974,
        byte_end: 1016,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::ValueType(4),
        byte_start: 1018,
        byte_end: 1147,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(10),
        byte_start: 1149,
        byte_end: 1194,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(11),
        byte_start: 1196,
        byte_end: 1242,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::ValueType(5),
        byte_start: 1244,
        byte_end: 1403,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(12),
        byte_start: 1405,
        byte_end: 1480,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(13),
        byte_start: 1482,
        byte_end: 1558,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(14),
        byte_start: 1559,
        byte_end: 1617,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::ValueType(6),
        byte_start: 1619,
        byte_end: 1772,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(15),
        byte_start: 1774,
        byte_end: 1843,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(16),
        byte_start: 1845,
        byte_end: 1915,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(17),
        byte_start: 1916,
        byte_end: 1972,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::ValueType(7),
        byte_start: 1974,
        byte_end: 2097,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(18),
        byte_start: 2099,
        byte_end: 2138,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(19),
        byte_start: 2140,
        byte_end: 2180,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::ValueType(8),
        byte_start: 2182,
        byte_end: 2305,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(20),
        byte_start: 2307,
        byte_end: 2346,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(21),
        byte_start: 2348,
        byte_end: 2388,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::ValueType(9),
        byte_start: 2390,
        byte_end: 2513,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(22),
        byte_start: 2515,
        byte_end: 2554,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(23),
        byte_start: 2556,
        byte_end: 2596,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::ValueType(10),
        byte_start: 2598,
        byte_end: 2731,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(24),
        byte_start: 2733,
        byte_end: 2782,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(25),
        byte_start: 2784,
        byte_end: 2834,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::ValueType(11),
        byte_start: 2836,
        byte_end: 2967,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(26),
        byte_start: 2969,
        byte_end: 3016,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(27),
        byte_start: 3018,
        byte_end: 3066,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::ValueType(12),
        byte_start: 3068,
        byte_end: 3189,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(28),
        byte_start: 3191,
        byte_end: 3230,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(29),
        byte_start: 3232,
        byte_end: 3272,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::ValueType(13),
        byte_start: 3274,
        byte_end: 3405,
    },
    CanonicalDeclarationFact {
        identity: CanonicalDeclarationIdentity::TypeBinding(30),
        byte_start: 3407,
        byte_end: 3462,
    },
];
const CANONICAL_VALUE_TYPE_ORIGIN_INDICES: [usize; 14] =
    [2, 6, 10, 13, 16, 19, 23, 27, 30, 33, 36, 39, 42, 45];
const CANONICAL_BINDING_ORIGIN_INDICES: [usize; 31] = [
    3, 4, 5, 7, 8, 9, 11, 12, 14, 15, 17, 18, 20, 21, 22, 24, 25, 26, 28, 29, 31, 32, 34, 35, 37,
    38, 40, 41, 43, 44, 46,
];

#[test]
fn checks_a_core_verified_non_golden_standard_source() {
    let snapshot = verified_standard_source_fixture();

    let checked = check_standard_library_source(&snapshot).unwrap();

    assert_eq!(
        checked.verified_snapshot().revision(),
        StandardLibraryRevisionId::from_bytes([7; 16])
    );
    assert_eq!(
        checked.verified_snapshot().digest(),
        Sha256Digest::from_bytes(SUCCESS_STANDARD_DIGEST)
    );
    assert_eq!(
        checked.verified_snapshot().source().id(),
        SourceRevisionId::from_bytes([6; 16])
    );
    assert_eq!(
        checked.verified_snapshot().catalogue().revision(),
        CatalogueRevisionId::from_bytes([8; 16])
    );
    assert_eq!(
        checked.verified_snapshot().source().units()[0].content(),
        "CREATE SCHEMA std;CREATE SCHEMA std.types;CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;"
    );

    assert_eq!(checked.schemas().len(), 2);
    assert_eq!(checked.schemas()[0].id(), SchemaId::from_bytes([1; 16]));
    assert_eq!(checked.schemas()[0].name(), &semantic_name(["std"]));
    assert_eq!(checked.schemas()[0].origin(), source_origin(0, 18));
    assert_eq!(checked.schemas()[1].id(), SchemaId::from_bytes([2; 16]));
    assert_eq!(
        checked.schemas()[1].name(),
        &semantic_name(["std", "types"])
    );
    assert_eq!(checked.schemas()[1].origin(), source_origin(18, 42));

    assert_eq!(checked.value_types().len(), 1);
    let value_type = &checked.value_types()[0];
    assert_eq!(value_type.id(), TypeId::from_bytes([3; 16]));
    assert_eq!(
        value_type.name(),
        &semantic_name(["std", "types", "boolean"])
    );
    assert_eq!(
        value_type.kind(),
        orna_core::catalogue::ValueTypeKind::Primitive
    );
    assert_eq!(value_type.mutability(), ValueTypeMutability::Immutable);
    assert_eq!(value_type.persistence(), ValueTypePersistence::Persistable);
    assert_eq!(
        value_type.representation_contract(),
        "orna.kernel.value.boolean@1"
    );
    assert_eq!(value_type.origin(), source_origin(42, 159));

    assert_eq!(checked.type_bindings().len(), 2);
    let qualified = &checked.type_bindings()[0];
    assert_eq!(
        qualified.id().to_bytes(),
        [
            0x53, 0xf1, 0x37, 0x1e, 0xaf, 0xef, 0x9a, 0xe5, 0x34, 0x7f, 0x15, 0x5c, 0xf1, 0xdd,
            0x4d, 0x31,
        ]
    );
    assert_eq!(
        qualified.kind(),
        orna_core::catalogue::TypeBindingKind::Qualified
    );
    assert_eq!(
        qualified.name(),
        &orna_core::catalogue::TypeLookupName::qualified(semantic_name(["std", "boolean"]))
    );
    assert_eq!(qualified.target(), TypeId::from_bytes([3; 16]));
    assert_eq!(qualified.origin(), source_origin(159, 204));

    let prelude = &checked.type_bindings()[1];
    assert_eq!(
        prelude.id().to_bytes(),
        [
            0xfc, 0x31, 0x05, 0xaf, 0xaf, 0x25, 0x20, 0xd7, 0xc7, 0x7c, 0xdd, 0x6b, 0x0e, 0xf8,
            0x15, 0xaa,
        ]
    );
    assert_eq!(
        prelude.kind(),
        orna_core::catalogue::TypeBindingKind::Prelude
    );
    assert_eq!(
        prelude.name(),
        &orna_core::catalogue::TypeLookupName::prelude(PreludeTypeName::new(["boolean"]).unwrap())
    );
    assert_eq!(prelude.target(), TypeId::from_bytes([3; 16]));
    assert_eq!(prelude.origin(), source_origin(204, 250));

    assert_eq!(checked.schemas()[0].clone(), checked.schemas()[0]);
    assert_eq!(value_type.clone(), *value_type);
    assert_eq!(qualified.clone(), *qualified);
}

#[test]
fn checks_the_accepted_canonical_standard_source() {
    let snapshot = verified_canonical_standard_source_fixture();

    let checked = check_standard_library_source(&snapshot).unwrap();

    assert_eq!(
        checked.verified_snapshot().revision().to_bytes(),
        CANONICAL_RESERVED_ID
    );
    assert_eq!(
        checked
            .verified_snapshot()
            .catalogue()
            .revision()
            .to_bytes(),
        CANONICAL_RESERVED_ID
    );
    assert_eq!(
        checked.verified_snapshot().source().id().to_bytes(),
        CANONICAL_RESERVED_ID
    );
    assert_eq!(
        checked.verified_snapshot().source().units()[0]
            .id()
            .to_bytes(),
        CANONICAL_RESERVED_ID
    );
    assert_eq!(
        checked.verified_snapshot().source().units()[0].logical_path(),
        "std/types.orna"
    );
    assert_eq!(
        checked.verified_snapshot().source().units()[0].content(),
        CANONICAL_STANDARD_SOURCE
    );
    assert_eq!(
        checked.verified_snapshot().digest().to_bytes(),
        CANONICAL_STANDARD_DIGEST
    );
    assert_eq!(CANONICAL_STANDARD_SOURCE.len(), 3463);

    assert_eq!(checked.schemas().len(), 2);
    assert_eq!(checked.value_types().len(), 14);
    assert_eq!(
        checked
            .type_bindings()
            .iter()
            .filter(|binding| binding.kind() == orna_core::catalogue::TypeBindingKind::Qualified)
            .count(),
        14
    );
    assert_eq!(
        checked
            .type_bindings()
            .iter()
            .filter(|binding| binding.kind() == orna_core::catalogue::TypeBindingKind::Prelude)
            .count(),
        17
    );

    for (index, schema) in checked.schemas().iter().enumerate() {
        let expected_name = ["std", "std.types"][index];
        assert_eq!(schema.id().to_bytes(), CANONICAL_SCHEMA_IDS[index]);
        assert_eq!(schema.name().to_string(), expected_name);
        assert_eq!(
            schema.origin(),
            canonical_source_origin(CANONICAL_DECLARATION_FACTS[index])
        );
    }
    for (index, value_type) in checked.value_types().iter().enumerate() {
        let expected = CANONICAL_VALUE_TYPE_FACTS[index];
        assert_eq!(value_type.id().to_bytes(), CANONICAL_TYPE_IDS[index]);
        assert_eq!(value_type.name().to_string(), expected.name);
        assert_eq!(value_type.kind(), expected.kind);
        assert_eq!(value_type.mutability(), ValueTypeMutability::Immutable);
        assert_eq!(value_type.persistence(), expected.persistence);
        assert_eq!(
            value_type.representation_contract(),
            expected.representation_contract
        );
        assert_eq!(
            value_type.origin(),
            canonical_source_origin(
                CANONICAL_DECLARATION_FACTS[CANONICAL_VALUE_TYPE_ORIGIN_INDICES[index]]
            )
        );
    }
    for (index, binding) in checked.type_bindings().iter().enumerate() {
        let expected = CANONICAL_BINDING_FACTS[index];
        assert_eq!(binding.id().to_bytes(), CANONICAL_BINDING_IDS[index]);
        assert_eq!(binding.kind(), expected.kind.catalogue_kind());
        assert_eq!(binding.name().to_string(), expected.name);
        assert_eq!(
            binding.target().to_bytes(),
            CANONICAL_TYPE_IDS[expected.target_type_index]
        );
        assert_eq!(
            binding.origin(),
            canonical_source_origin(
                CANONICAL_DECLARATION_FACTS[CANONICAL_BINDING_ORIGIN_INDICES[index]]
            )
        );
    }
}

#[test]
fn checks_an_empty_application_against_a_checked_standard_library() {
    let standard = check_standard_library_source(&verified_standard_source_fixture()).unwrap();
    let application = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x2a; 16]),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", "")]).unwrap();

    let report = check_standard_application(&bundle, &context);

    assert_eq!(report.diagnostics(), &[]);
    assert_eq!(
        report.standard_library().verified_snapshot().digest(),
        standard.verified_snapshot().digest()
    );
    let checked = report.checked_bundle().unwrap();
    assert_eq!(
        checked.base_catalogue_revision(),
        CatalogueRevisionId::from_bytes([0x2a; 16])
    );
    assert!(checked.uses().is_empty());
    assert!(checked.standard_type_references().is_empty());
}

#[test]
fn checks_one_new_application_against_the_empty_sentinel_catalogue() {
    let standard =
        check_standard_library_source(&verified_canonical_standard_source_fixture()).unwrap();
    let source = "CREATE SCHEMA app; CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL);";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();

    let report = check_new_application(&bundle, &standard).unwrap();

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    assert_eq!(
        checked.base_catalogue_revision(),
        EMPTY_APPLICATION_CATALOGUE_REVISION_ID
    );
    assert_eq!(EMPTY_APPLICATION_CATALOGUE_REVISION_ID.to_bytes(), [0; 16]);
    assert_eq!(checked.object_types().count(), 1);
    assert_eq!(checked.uses().len(), 1);
}

#[test]
fn checks_new_application_source_diagnostics_after_authority_establishment() {
    let standard = check_standard_library_source(&verified_standard_source_fixture()).unwrap();
    let bundle =
        SourceBundle::new([SourceUnit::new("application.orna", "CREATE SCHEMA ;")]).unwrap();
    let expected_diagnostics = parse_bundle(&bundle).diagnostics().to_vec();

    let report = check_new_application(&bundle, &standard).unwrap();

    assert_eq!(report.diagnostics(), expected_diagnostics);
    assert!(report.checked_bundle().is_none());
}

#[test]
fn resolves_a_prelude_standard_value_type_and_retains_its_durable_identity() {
    let standard = check_standard_library_source(&verified_standard_source_fixture()).unwrap();
    let application = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x2b; 16]),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app; CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL);";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();

    let report = check_standard_application(&bundle, &context);

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    let objects = checked.object_types().collect::<Vec<_>>();
    assert_eq!(objects.len(), 1);
    let fields = objects[0].fields().collect::<Vec<_>>();
    assert_eq!(fields.len(), 1);
    let resolved = fields[0].resolved_type();
    assert!(resolved.value().is_some());
    let Some(value) = resolved.value() else {
        return;
    };
    assert_eq!(value.type_id(), TypeId::from_bytes([3; 16]));
    assert_eq!(
        value.kind(),
        CheckedTypeUseKind::Field {
            owner: objects[0].id(),
            field: fields[0].id(),
        }
    );
    let start = source.find("BOOLEAN").unwrap();
    assert_eq!(value.location().logical_path(), "application.orna");
    assert_eq!(value.location().span().start(), start);
    assert_eq!(value.location().span().end(), start + "BOOLEAN".len());
}

#[test]
fn records_standard_server_and_client_signature_uses_without_accepting_client_parameters() {
    let standard = check_standard_library_source(&verified_standard_source_fixture()).unwrap();
    let application = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x2c; 16]),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app;\
            CREATE TYPE app.flag AS OBJECT (value BOOLEAN);\
            CREATE SERVER FUNCTION app.read(p_flag REF app.flag) RETURNS ROWS (value BOOLEAN) \
            TRANSACTION READ ONLY VOLATILITY STABLE AS SELECT f.value FROM app.flag f \
            WHERE REF(f) = p_flag;\
            CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();

    let report = check_standard_application(&bundle, &context);

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();

    let objects = checked.object_types().collect::<Vec<_>>();
    let servers = checked.server_functions().collect::<Vec<_>>();
    let clients = checked.client_functions().collect::<Vec<_>>();
    assert_eq!(objects.len(), 1);
    assert_eq!(servers.len(), 1);
    assert_eq!(clients.len(), 1);

    let fields = objects[0].fields().collect::<Vec<_>>();
    let parameters = servers[0].parameters().collect::<Vec<_>>();
    let columns = servers[0].return_columns().collect::<Vec<_>>();
    assert_eq!(fields.len(), 1);
    assert_eq!(parameters.len(), 1);
    assert_eq!(columns.len(), 1);
    assert_eq!(clients[0].parameters().count(), 0);

    let declaration_uses = checked
        .uses()
        .iter()
        .filter(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Field { .. }
                    | CheckedTypeUseKind::Parameter { .. }
                    | CheckedTypeUseKind::Return { .. }
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(declaration_uses.len(), 4);
    assert_eq!(
        declaration_uses
            .iter()
            .map(|type_use| type_use.kind())
            .collect::<Vec<_>>(),
        [
            CheckedTypeUseKind::Field {
                owner: objects[0].id(),
                field: fields[0].id(),
            },
            CheckedTypeUseKind::Parameter {
                owner: servers[0].id(),
                parameter: parameters[0].id(),
            },
            CheckedTypeUseKind::Return {
                owner: servers[0].id(),
                ordinal: 0,
            },
            CheckedTypeUseKind::Return {
                owner: clients[0].id(),
                ordinal: 0,
            },
        ]
    );

    assert_eq!(
        fields[0]
            .resolved_type()
            .value()
            .map(CheckedValueTypeUse::type_id),
        Some(TypeId::from_bytes([3; 16]))
    );
    assert!(parameters[0].resolved_type().object_reference().is_some());
    assert_eq!(
        columns[0]
            .resolved_type()
            .value()
            .map(CheckedValueTypeUse::type_id),
        Some(TypeId::from_bytes([3; 16]))
    );
    assert_eq!(
        clients[0]
            .return_type()
            .value()
            .map(CheckedValueTypeUse::type_id),
        Some(TypeId::from_bytes([3; 16]))
    );
    let reference_start = source.find("app.flag) RETURNS").unwrap();
    assert_eq!(
        declaration_uses[1].location().span().start(),
        reference_start
    );

    let debug_values = [
        format!("{report:?}"),
        format!("{checked:?}"),
        format!("{:?}", objects[0]),
        format!("{:?}", fields[0]),
        format!("{:?}", servers[0]),
        format!("{:?}", parameters[0]),
        format!("{:?}", columns[0]),
        format!("{:?}", clients[0]),
    ];
    for rendered in debug_values {
        assert!(!rendered.contains("SemanticType"));
        assert!(!rendered.contains("Scalar"));
    }
}

#[test]
fn preserves_the_client_parameter_diagnostic_with_standard_authority() {
    let standard = check_standard_library_source(&verified_standard_source_fixture()).unwrap();
    let application = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x2d; 16]),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled(value BOOLEAN) RETURNS BOOLEAN RETURN TRUE;";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();

    let report = check_standard_application(&bundle, &context);
    let legacy_report = check(&bundle, &application);

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].message(),
        "this CLIENT function cannot declare parameters yet"
    );
    assert_eq!(report.diagnostics(), legacy_report.diagnostics());
    assert!(report.checked_bundle().is_none());
}

#[test]
fn resolves_every_accepted_canonical_standard_spelling_to_its_checked_type_id() {
    let standard =
        check_standard_library_source(&verified_canonical_standard_source_fixture()).unwrap();
    let application = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x2e; 16]),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let prelude = [
        ("BOOLEAN", 0),
        ("BOOL", 0),
        ("INTEGER", 1),
        ("INT", 1),
        ("BIGINT", 2),
        ("FLOAT", 3),
        ("DECIMAL", 4),
        ("CHARACTER LARGE OBJECT", 5),
        ("TEXT", 5),
        ("BINARY LARGE OBJECT", 6),
        ("BYTES", 6),
        ("UUID", 7),
        ("DATE", 8),
        ("TIME", 9),
        ("TIMESTAMP", 10),
        ("DURATION", 11),
        ("VOID", 12),
    ];
    let primaries = [
        ("std.types.BOOLEAN", 0),
        ("std.types.INTEGER", 1),
        ("std.types.BIGINT", 2),
        ("std.types.FLOAT", 3),
        ("std.types.DECIMAL", 4),
        ("std.types.CHARACTER_LARGE_OBJECT", 5),
        ("std.types.BINARY_LARGE_OBJECT", 6),
        ("std.types.UUID", 7),
        ("std.types.DATE", 8),
        ("std.types.TIME", 9),
        ("std.types.TIMESTAMP", 10),
        ("std.types.DURATION", 11),
        ("std.types.VOID", 12),
    ];
    let qualified = [
        ("std.BOOLEAN", 0),
        ("std.INTEGER", 1),
        ("std.BIGINT", 2),
        ("std.FLOAT", 3),
        ("std.DECIMAL", 4),
        ("std.CHARACTER_LARGE_OBJECT", 5),
        ("std.BINARY_LARGE_OBJECT", 6),
        ("std.UUID", 7),
        ("std.DATE", 8),
        ("std.TIME", 9),
        ("std.TIMESTAMP", 10),
        ("std.DURATION", 11),
        ("std.VOID", 12),
    ];
    let spellings = prelude
        .into_iter()
        .chain(primaries)
        .chain(qualified)
        .collect::<Vec<_>>();
    let fields = spellings
        .iter()
        .enumerate()
        .map(|(index, (spelling, _))| format!("f{index} {spelling}"))
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!("CREATE SCHEMA app; CREATE TYPE app.all_types AS OBJECT ({fields});");
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();

    let report = check_standard_application(&bundle, &context);

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    assert_eq!(checked.uses().len(), 43);
    for (type_use, (_, expected_type_index)) in checked.uses().iter().zip(spellings) {
        assert!(type_use.value().is_some());
        let Some(value) = type_use.value() else {
            return;
        };
        assert_eq!(
            value.type_id(),
            TypeId::from_bytes(CANONICAL_TYPE_IDS[expected_type_index])
        );
    }
}

#[test]
fn standard_application_context_checks_every_adjacent_gate_precedence_and_error_contract() {
    let standard = check_standard_library_source(&verified_standard_source_fixture()).unwrap();

    let schema_identity_before_schema_name = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x31; 16]),
        vec![
            SchemaDefinition::new(SchemaId::from_bytes([0x32; 16]), semantic_name(["std"])),
            SchemaDefinition::new(
                SchemaId::from_bytes([2; 16]),
                semantic_name(["application"]),
            ),
        ],
        Vec::new(),
    )
    .unwrap();
    assert_standard_context_error(
        StandardApplicationCheckContext::try_new(&schema_identity_before_schema_name, &standard),
        StandardApplicationContextError::SchemaIdentityConflict {
            id: SchemaId::from_bytes([2; 16]),
        },
        format!(
            "the application catalogue conflicts with standard schema identity {}",
            SchemaId::from_bytes([2; 16])
        ),
    );

    let type_after_schema_name = ValueTypeDefinition::primitive(
        TypeId::from_bytes([3; 16]),
        semantic_name(["application", "flag"]),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.boolean@1",
    );
    let schema_name_before_type_identity = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0x32; 16]),
        vec![
            SchemaDefinition::new(
                SchemaId::from_bytes([0x33; 16]),
                semantic_name(["std", "types"]),
            ),
            SchemaDefinition::new(
                SchemaId::from_bytes([0x34; 16]),
                semantic_name(["application"]),
            ),
        ],
        Vec::new(),
        vec![type_after_schema_name],
        Vec::new(),
    )
    .unwrap();
    assert_standard_context_error(
        StandardApplicationCheckContext::try_new(&schema_name_before_type_identity, &standard),
        StandardApplicationContextError::SchemaNameConflict {
            name: semantic_name(["std", "types"]),
        },
        "the application catalogue conflicts with standard schema name std.types".to_owned(),
    );

    let type_identity = ValueTypeDefinition::primitive(
        TypeId::from_bytes([3; 16]),
        semantic_name(["application", "flag"]),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.boolean@1",
    );
    let binding_after_type_identity = TypeBinding::prelude(
        PreludeTypeName::new(["boolean"]).unwrap(),
        type_identity.id(),
    )
    .unwrap();
    let type_identity_before_binding_identity = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0x34; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x35; 16]),
            semantic_name(["application"]),
        )],
        Vec::new(),
        vec![type_identity],
        vec![binding_after_type_identity],
    )
    .unwrap();
    assert_standard_context_error(
        StandardApplicationCheckContext::try_new(&type_identity_before_binding_identity, &standard),
        StandardApplicationContextError::TypeIdentityConflict {
            id: TypeId::from_bytes([3; 16]),
        },
        format!(
            "the application catalogue conflicts with standard type identity {}",
            TypeId::from_bytes([3; 16])
        ),
    );

    let binding_target = ValueTypeDefinition::primitive(
        TypeId::from_bytes([0x36; 16]),
        semantic_name(["application", "flag"]),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.boolean@1",
    );
    let binding = TypeBinding::prelude(
        PreludeTypeName::new(["boolean"]).unwrap(),
        binding_target.id(),
    )
    .unwrap();
    let binding_identity_before_unsupported_contract = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0x37; 16]),
        vec![SchemaDefinition::new(
            SchemaId::from_bytes([0x38; 16]),
            semantic_name(["application"]),
        )],
        Vec::new(),
        vec![binding_target],
        vec![binding.clone()],
    )
    .unwrap();
    let unsupported_standard =
        super::resolver::checked_standard_library_with_contract_overrides_for_test(
            &verified_standard_source_fixture(),
            &[(0, "unsupported@1")],
        )
        .unwrap();
    assert_standard_context_error(
        StandardApplicationCheckContext::try_new(
            &binding_identity_before_unsupported_contract,
            &unsupported_standard,
        ),
        StandardApplicationContextError::TypeBindingIdentityConflict { id: binding.id() },
        format!(
            "the application catalogue conflicts with standard type binding identity {}",
            binding.id()
        ),
    );

    let unsupported_before_duplicate =
        super::resolver::checked_standard_library_with_contract_overrides_for_test(
            &verified_canonical_standard_source_fixture(),
            &[
                (0, "unsupported-first@1"),
                (1, "orna.kernel.value.boolean@1"),
            ],
        )
        .unwrap();
    let empty_application = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x38; 16]),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    assert_standard_context_error(
        StandardApplicationCheckContext::try_new(&empty_application, &unsupported_before_duplicate),
        StandardApplicationContextError::UnsupportedCompatibilityContract {
            type_id: TypeId::from_bytes(CANONICAL_TYPE_IDS[0]),
            contract: "unsupported-first@1".to_owned(),
        },
        format!(
            "the standard value type {} uses unsupported compatibility contract unsupported-first@1",
            TypeId::from_bytes(CANONICAL_TYPE_IDS[0])
        ),
    );

    let duplicate_contract =
        super::resolver::checked_standard_library_with_contract_overrides_for_test(
            &verified_canonical_standard_source_fixture(),
            &[(1, "orna.kernel.value.boolean@1")],
        )
        .unwrap();
    assert_standard_context_error(
            StandardApplicationCheckContext::try_new(&empty_application, &duplicate_contract),
            StandardApplicationContextError::CompatibilityContractConflict {
                contract: "orna.kernel.value.boolean@1".to_owned(),
            },
            "the standard library uses compatibility contract orna.kernel.value.boolean@1 for more than one type".to_owned(),
        );
}

#[test]
fn quoted_names_do_not_acquire_standard_prelude_meaning() {
    let standard = check_standard_library_source(&verified_standard_source_fixture()).unwrap();
    let application = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x39; 16]),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app; CREATE TYPE app.flag AS OBJECT (value \"BOOLEAN\");";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();

    let report = check_standard_application(&bundle, &context);

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::UnknownQualifiedName
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "unknown type name BOOLEAN"
    );
    assert!(report.checked_bundle().is_none());
}

#[test]
fn unknown_qualified_aliases_and_quoted_counterparts_do_not_resolve_through_standard() {
    let standard = check_standard_library_source(&verified_standard_source_fixture()).unwrap();
    let application = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x3a; 16]),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app; CREATE TYPE app.flag AS OBJECT (alias std.ALIAS, quoted std.\"BOOLEAN\");";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();

    let report = check_standard_application(&bundle, &context);

    assert_eq!(report.diagnostics().len(), 2);
    let expected = [
        ("std.ALIAS", "unknown type name std.alias"),
        ("std.\"BOOLEAN\"", "unknown type name std.BOOLEAN"),
    ];
    for (diagnostic, (spelling, message)) in report.diagnostics().iter().zip(expected) {
        assert_eq!(diagnostic.code(), DiagnosticCode::UnknownQualifiedName);
        assert_eq!(diagnostic.message(), message);
        let start = source.find(spelling).unwrap();
        assert_eq!(diagnostic.location().span().start(), start);
        assert_eq!(diagnostic.location().span().end(), start + spelling.len());
    }
    assert!(report.checked_bundle().is_none());
}

#[test]
fn standard_declaration_type_uses_are_ordered_by_written_source_not_declaration_family() {
    let standard = check_standard_library_source(&verified_standard_source_fixture()).unwrap();
    let application = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x3a; 16]),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app;\
            CREATE SERVER FUNCTION app.read() RETURNS ROWS (value BOOLEAN) \
            AS SELECT f.value FROM app.flag f;\
            CREATE TYPE app.flag AS OBJECT (value BOOLEAN);";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();

    let report = check_standard_application(&bundle, &context);

    assert_eq!(report.diagnostics(), &[]);
    let checked = report.checked_bundle().unwrap();
    let declaration_uses = checked
        .uses()
        .iter()
        .filter(|type_use| {
            matches!(
                type_use.kind(),
                CheckedTypeUseKind::Field { .. }
                    | CheckedTypeUseKind::Parameter { .. }
                    | CheckedTypeUseKind::Return { .. }
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(declaration_uses.len(), 2);
    assert!(matches!(
        declaration_uses[0].kind(),
        CheckedTypeUseKind::Return { .. }
    ));
    assert!(matches!(
        declaration_uses[1].kind(),
        CheckedTypeUseKind::Field { .. }
    ));
    assert!(
        declaration_uses[0].location().span().start()
            < declaration_uses[1].location().span().start()
    );
}

#[test]
fn standard_values_are_not_valid_ref_targets() {
    let standard = check_standard_library_source(&verified_standard_source_fixture()).unwrap();
    let application = CatalogueSnapshot::new(
        CatalogueRevisionId::from_bytes([0x3b; 16]),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let context = StandardApplicationCheckContext::try_new(&application, &standard).unwrap();
    let source = "CREATE SCHEMA app; CREATE TYPE app.flag AS OBJECT (value REF std.BOOLEAN);";
    let bundle = SourceBundle::new([SourceUnit::new("application.orna", source)]).unwrap();

    let report = check_standard_application(&bundle, &context);

    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::InvalidReferenceTarget
    );
    assert_eq!(
        report.diagnostics()[0].message(),
        "REF target std.boolean is a scalar type"
    );
    let start = source.find("std.BOOLEAN").unwrap();
    assert_eq!(report.diagnostics()[0].location().span().start(), start);
    assert_eq!(
        report.diagnostics()[0].location().span().end(),
        start + "std.BOOLEAN".len()
    );
    assert!(report.checked_bundle().is_none());
}

#[test]
fn counts_source_units_before_parsing_or_reconciling() {
    let empty = verified_empty_catalogue_fixture(
        &[],
        [
            0x82, 0xba, 0xf7, 0xee, 0xe1, 0x53, 0x2a, 0x90, 0xa8, 0x10, 0x14, 0x71, 0x0d, 0xa6,
            0xb7, 0x07, 0xb0, 0x37, 0x24, 0xe6, 0x62, 0xce, 0x4b, 0x3a, 0xe7, 0x07, 0xa4, 0xc7,
            0x78, 0xd3, 0xe9, 0xb1,
        ],
    );
    let two = verified_empty_catalogue_fixture(
        &[
            ("std/one.orna", "CREATE SCHEMA std.;"),
            ("std/two.orna", ""),
        ],
        [
            0x71, 0xf0, 0x0f, 0x75, 0x50, 0x39, 0xd7, 0xa5, 0x54, 0xc4, 0xec, 0xb1, 0x80, 0x92,
            0xdf, 0x21, 0xe3, 0x30, 0xc0, 0x28, 0x69, 0xd7, 0xa1, 0xbd, 0xa9, 0xbc, 0x8e, 0xb2,
            0xbe, 0x32, 0x3e, 0xba,
        ],
    );

    assert_eq!(
        check_standard_library_source(&empty).unwrap_err(),
        StandardLibraryCheckError::SourceUnitCount { actual: 0 }
    );
    assert_eq!(
        check_standard_library_source(&two).unwrap_err(),
        StandardLibraryCheckError::SourceUnitCount { actual: 2 }
    );
}

#[test]
fn returns_parser_diagnostics_before_reconciliation() {
    const SOURCE: &str = "CREATE SCHEMA std.;CREATE SCHEMA ;CREATE SCHEMA std;";
    let parsed =
        parse_bundle(&SourceBundle::new([SourceUnit::new("std/types.orna", SOURCE)]).unwrap());
    assert_eq!(parsed.units()[0].parsed().schemas().len(), 1);
    assert_eq!(parsed.diagnostics().len(), 2);

    let snapshot = verified_empty_catalogue_fixture(
        &[("std/types.orna", SOURCE)],
        [
            0xe8, 0x6d, 0xd0, 0x09, 0x63, 0x3e, 0xa6, 0x94, 0xf3, 0x7d, 0xe5, 0xd6, 0xcc, 0x97,
            0x34, 0xc8, 0x4f, 0x9a, 0x72, 0xb8, 0x0b, 0x4e, 0xbb, 0x3f, 0x96, 0x03, 0x5d, 0xf8,
            0x40, 0x7a, 0x22, 0x60,
        ],
    );

    let error = check_standard_library_source(&snapshot).unwrap_err();

    assert!(matches!(
        error,
        StandardLibraryCheckError::Diagnostics { .. }
    ));
    let StandardLibraryCheckError::Diagnostics { diagnostics } = error else {
        return;
    };
    assert_eq!(diagnostics, parsed.diagnostics());
}

#[test]
fn standard_source_check_errors_have_the_documented_messages_and_no_causes() {
    let errors = [
        StandardLibraryCheckError::SourceUnitCount { actual: 9 },
        StandardLibraryCheckError::Diagnostics {
            diagnostics: Vec::new(),
        },
        StandardLibraryCheckError::SourceMismatch,
    ];
    let messages = [
        "the verified standard library has 9 source units, expected exactly one",
        "the verified standard library source has compiler diagnostics",
        "the verified standard library source does not match its catalogue and origins",
    ];

    for (error, message) in errors.iter().zip(messages) {
        assert_eq!(error.to_string(), message);
        assert!(std::error::Error::source(error).is_none());
    }
}

fn empty_version_two_active(
    standard: &orna_core::revision::VerifiedStandardLibrarySnapshot,
) -> ActiveDatabaseRevision {
    let source_unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0x41; 16]),
        0,
        "active.orna",
        "",
        source_unit_content_digest("").unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([0x42; 16]),
        SourceRevisionId::from_bytes([0x43; 16]),
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([0x42; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0x44; 16]),
        vec![],
        vec![],
        vec![],
        vec![],
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

fn empty_version_one_active() -> ActiveDatabaseRevision {
    let source_unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0xf1; 16]),
        0,
        "active.orna",
        "",
        source_unit_content_digest("").unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([0xf2; 16]),
        SourceRevisionId::from_bytes([0xf3; 16]),
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([0xf2; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([0xf4; 16]),
        vec![],
        vec![],
        vec![],
        vec![],
    )
    .unwrap();
    let catalogue_hash = catalogue_digest(&catalogue, &[], &[], &[], &[]).unwrap();
    ActiveDatabaseRevision::new(
        RevisionPair::new(source.id(), catalogue.revision()),
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

fn version_one_active_with(
    source_text: &str,
    catalogue: CatalogueSnapshot,
) -> ActiveDatabaseRevision {
    version_one_active_with_origins(source_text, catalogue, Vec::new())
}

fn version_one_active_with_origins(
    source_text: &str,
    catalogue: CatalogueSnapshot,
    origins: Vec<DefinitionOrigin>,
) -> ActiveDatabaseRevision {
    let source_unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes([0xa1; 16]),
        0,
        "active.orna",
        source_text,
        source_unit_content_digest(source_text).unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([0xa2; 16]),
        SourceRevisionId::from_bytes([0xa3; 16]),
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([0xa2; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let catalogue_hash = catalogue_digest(&catalogue, &[], &[], &origins, &[]).unwrap();
    ActiveDatabaseRevision::new(
        RevisionPair::new(source.id(), catalogue.revision()),
        source,
        catalogue,
        catalogue_hash,
        Vec::new(),
        Vec::new(),
        origins,
        Vec::new(),
    )
    .unwrap()
}

fn stored_source_with_ids(
    source_text: &str,
    unit_id: SourceUnitId,
    bundle_id: SourceBundleId,
    revision_id: SourceRevisionId,
) -> StoredSourceRevision {
    let source_unit = StoredSourceUnit::new(
        unit_id,
        0,
        "active.orna",
        source_text,
        source_unit_content_digest(source_text).unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
    StoredSourceRevision::new(
        bundle_id,
        revision_id,
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(bundle_id, None, bundle_hash).unwrap(),
    )
    .unwrap()
}

fn version_one_active_with_source(
    source: StoredSourceRevision,
    catalogue: CatalogueSnapshot,
    origins: Vec<DefinitionOrigin>,
) -> ActiveDatabaseRevision {
    let catalogue_hash = catalogue_digest(&catalogue, &[], &[], &origins, &[]).unwrap();
    ActiveDatabaseRevision::new(
        RevisionPair::new(source.id(), catalogue.revision()),
        source,
        catalogue,
        catalogue_hash,
        Vec::new(),
        Vec::new(),
        origins,
        Vec::new(),
    )
    .unwrap()
}

fn active_from_prepared_standard_candidate(
    prepared: &DeployableRevision,
    historical_function_revisions: Vec<orna_core::revision::FunctionRevisionRecord>,
) -> ActiveDatabaseRevision {
    let current_function_revisions = prepared
        .current_function_revisions()
        .map_or_else(Vec::new, ToOwned::to_owned);
    ActiveDatabaseRevision::new_with_catalogue_hash_context(
        ActiveDatabaseRevisionInput::new(
            prepared.candidate_pair(),
            prepared.source().clone(),
            prepared.candidate().clone(),
            prepared.catalogue_hash(),
            ActiveRevisionContent::new(
                prepared.expressions().to_vec(),
                current_function_revisions,
                prepared.origins().to_vec(),
                prepared.references().to_vec(),
            )
            .with_history(historical_function_revisions),
        ),
        prepared.catalogue_hash_context().clone(),
    )
    .unwrap()
}

fn active_from_prepared_version_one_candidate(
    prepared: &DeployableRevision,
) -> ActiveDatabaseRevision {
    ActiveDatabaseRevision::new(
        prepared.candidate_pair(),
        prepared.source().clone(),
        prepared.candidate().clone(),
        prepared.catalogue_hash(),
        prepared.expressions().to_vec(),
        prepared.new_function_revisions().to_vec(),
        prepared.origins().to_vec(),
        prepared.references().to_vec(),
    )
    .unwrap()
}

fn version_one_client_active_from_standard_candidate(
    prepared: &DeployableRevision,
) -> ActiveDatabaseRevision {
    const STANDARD_BOOLEAN: TypeId = TypeId::from_bytes([3; 16]);
    let candidate = prepared.candidate();
    assert!(candidate.object_types().is_empty());
    assert_eq!(candidate.functions().len(), 1);
    let function = &candidate.functions()[0];
    assert_eq!(function.domain(), FunctionDomain::Client);
    assert!(function.parameters().is_empty());
    assert_eq!(
        function.return_type(),
        &FunctionReturn::Single(ResolvedType::Value(STANDARD_BOOLEAN))
    );
    let legacy_function = FunctionDefinition::new(
        function.id(),
        function.name().clone(),
        function.domain(),
        Vec::new(),
        FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Boolean)),
        function.current_revision(),
        function.security(),
        function.transaction(),
        function.volatility(),
    );
    let legacy_catalogue = CatalogueSnapshot::new_with_functions_and_types(
        candidate.revision(),
        candidate.schemas().to_vec(),
        Vec::new(),
        candidate.value_types().to_vec(),
        candidate.type_bindings().to_vec(),
        vec![legacy_function],
    )
    .unwrap();
    let legacy_function = &legacy_catalogue.functions()[0];
    let current = prepared.current_function_revisions().unwrap_or_default();
    assert_eq!(current.len(), 1);
    let revision = &current[0];
    let version_one_revision = FunctionRevisionRecord::new(
        revision.function(),
        revision.id(),
        revision.revision_number(),
        revision.declaration_origin(),
        revision.declaration_content_hash(),
        function_semantic_digest(
            legacy_function,
            revision.language_version(),
            revision.artifact(),
            prepared.expressions(),
            &[],
        )
        .unwrap(),
        revision.language_version(),
        revision.artifact().clone(),
    )
    .unwrap();
    let catalogue_hash = catalogue_digest(
        &legacy_catalogue,
        std::slice::from_ref(&version_one_revision),
        prepared.expressions(),
        prepared.origins(),
        &[],
    )
    .unwrap();
    ActiveDatabaseRevision::new(
        prepared.candidate_pair(),
        prepared.source().clone(),
        legacy_catalogue,
        catalogue_hash,
        prepared.expressions().to_vec(),
        vec![version_one_revision],
        prepared.origins().to_vec(),
        Vec::new(),
    )
    .unwrap()
}

fn active_with_history(
    active: &ActiveDatabaseRevision,
    historical_function_revisions: Vec<FunctionRevisionRecord>,
) -> ActiveDatabaseRevision {
    ActiveDatabaseRevision::new_with_history(
        active.pair(),
        active.source().clone(),
        active.catalogue().clone(),
        active.catalogue_hash(),
        active.expressions().to_vec(),
        active.function_revisions().to_vec(),
        historical_function_revisions,
        active.origins().to_vec(),
        active.references().to_vec(),
    )
    .unwrap()
}

fn verified_standard_source_fixture() -> orna_core::revision::VerifiedStandardLibrarySnapshot {
    const SOURCE: &str = "CREATE SCHEMA std;CREATE SCHEMA std.types;CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;EXPORT TYPE std.types.BOOLEAN AS std.BOOLEAN;EXPORT TYPE std.BOOLEAN TO PRELUDE AS BOOLEAN;";

    let source_unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes(CANONICAL_RESERVED_ID),
        0,
        "std/types.orna",
        SOURCE,
        source_unit_content_digest(SOURCE).unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([5; 16]),
        SourceRevisionId::from_bytes([6; 16]),
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([5; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();

    let boolean = ValueTypeDefinition::primitive(
        TypeId::from_bytes([3; 16]),
        semantic_name(["std", "types", "boolean"]),
        ValueTypeMutability::Immutable,
        ValueTypePersistence::Persistable,
        "orna.kernel.value.boolean@1",
    );
    let qualified =
        TypeBinding::qualified(semantic_name(["std", "boolean"]), boolean.id()).unwrap();
    let prelude =
        TypeBinding::prelude(PreludeTypeName::new(["boolean"]).unwrap(), boolean.id()).unwrap();
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([8; 16]),
        vec![
            SchemaDefinition::new(SchemaId::from_bytes([1; 16]), semantic_name(["std"])),
            SchemaDefinition::new(
                SchemaId::from_bytes([2; 16]),
                semantic_name(["std", "types"]),
            ),
        ],
        vec![],
        vec![boolean],
        vec![qualified.clone(), prelude.clone()],
    )
    .unwrap();
    let origins = vec![
        standard_origin(
            DefinitionIdentity::Schema(SchemaId::from_bytes([1; 16])),
            0,
            18,
        ),
        standard_origin(
            DefinitionIdentity::Schema(SchemaId::from_bytes([2; 16])),
            18,
            42,
        ),
        standard_origin(
            DefinitionIdentity::ValueType(TypeId::from_bytes([3; 16])),
            42,
            159,
        ),
        standard_origin(DefinitionIdentity::TypeBinding(qualified.id()), 159, 204),
        standard_origin(DefinitionIdentity::TypeBinding(prelude.id()), 204, 250),
    ];
    let snapshot = StandardLibrarySnapshot::new(
        StandardLibraryRevisionId::from_bytes([7; 16]),
        StandardLibraryDigestVersion::Version1,
        source,
        "orna.language/1",
        catalogue,
        origins,
        Sha256Digest::from_bytes(SUCCESS_STANDARD_DIGEST),
    )
    .unwrap();

    verify_standard_library_snapshot(snapshot).unwrap()
}

fn verified_non_std_schema_standard_source_fixture()
-> orna_core::revision::VerifiedStandardLibrarySnapshot {
    const SOURCE: &str = "CREATE SCHEMA library;";
    let source_unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes(CANONICAL_RESERVED_ID),
        0,
        "std/types.orna",
        SOURCE,
        source_unit_content_digest(SOURCE).unwrap(),
    )
    .unwrap();
    let source_bundle = SourceBundleId::from_bytes([91; 16]);
    let source_revision = SourceRevisionId::from_bytes([92; 16]);
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
    let source = StoredSourceRevision::new(
        source_bundle,
        source_revision,
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(source_bundle, None, bundle_hash).unwrap(),
    )
    .unwrap();
    let schema_id = SchemaId::from_bytes([93; 16]);
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([94; 16]),
        vec![SchemaDefinition::new(schema_id, semantic_name(["library"]))],
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let origins = vec![DefinitionOrigin::new(
        DefinitionIdentity::Schema(schema_id),
        SourceOrigin::new(
            SourceUnitId::from_bytes(CANONICAL_RESERVED_ID),
            0,
            SOURCE.len() as u32,
        )
        .unwrap(),
    )];
    let snapshot = StandardLibrarySnapshot::new(
        StandardLibraryRevisionId::from_bytes([95; 16]),
        StandardLibraryDigestVersion::Version1,
        source,
        "orna.language/1",
        catalogue,
        origins,
        Sha256Digest::from_bytes(NON_STD_SCHEMA_STANDARD_DIGEST),
    )
    .unwrap();

    verify_standard_library_snapshot(snapshot).unwrap()
}

fn verified_canonical_standard_source_fixture()
-> orna_core::revision::VerifiedStandardLibrarySnapshot {
    let source_unit = StoredSourceUnit::new(
        SourceUnitId::from_bytes(CANONICAL_RESERVED_ID),
        0,
        "std/types.orna",
        CANONICAL_STANDARD_SOURCE,
        source_unit_content_digest(CANONICAL_STANDARD_SOURCE).unwrap(),
    )
    .unwrap();
    let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes(CANONICAL_RESERVED_ID),
        SourceRevisionId::from_bytes(CANONICAL_RESERVED_ID),
        None,
        vec![source_unit],
        bundle_hash,
        source_revision_record_digest(
            SourceBundleId::from_bytes(CANONICAL_RESERVED_ID),
            None,
            bundle_hash,
        )
        .unwrap(),
    )
    .unwrap();
    let value_types = CANONICAL_VALUE_TYPE_FACTS
        .iter()
        .enumerate()
        .map(|(index, fact)| {
            let id = TypeId::from_bytes(CANONICAL_TYPE_IDS[index]);
            let name = semantic_name_from_dotted(fact.name);
            match fact.kind {
                ValueTypeKind::Primitive => ValueTypeDefinition::primitive(
                    id,
                    name,
                    ValueTypeMutability::Immutable,
                    fact.persistence,
                    fact.representation_contract,
                ),
                ValueTypeKind::Opaque => {
                    ValueTypeDefinition::opaque(id, name, fact.representation_contract)
                }
                _ => {
                    unreachable!("the canonical fixture contains only primitive and opaque values")
                }
            }
        })
        .collect();
    let type_bindings = CANONICAL_BINDING_FACTS
        .iter()
        .map(|fact| {
            let target = TypeId::from_bytes(CANONICAL_TYPE_IDS[fact.target_type_index]);
            match fact.kind {
                CanonicalBindingKind::Qualified => {
                    TypeBinding::qualified(semantic_name_from_dotted(fact.name), target)
                }
                CanonicalBindingKind::Prelude => TypeBinding::prelude(
                    PreludeTypeName::new(fact.name.split(' ')).unwrap(),
                    target,
                ),
            }
        })
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes(CANONICAL_RESERVED_ID),
        vec![
            SchemaDefinition::new(
                SchemaId::from_bytes(CANONICAL_SCHEMA_IDS[0]),
                semantic_name_from_dotted("std"),
            ),
            SchemaDefinition::new(
                SchemaId::from_bytes(CANONICAL_SCHEMA_IDS[1]),
                semantic_name_from_dotted("std.types"),
            ),
        ],
        vec![],
        value_types,
        type_bindings,
    )
    .unwrap();
    let snapshot = StandardLibrarySnapshot::new(
        StandardLibraryRevisionId::from_bytes(CANONICAL_RESERVED_ID),
        StandardLibraryDigestVersion::Version1,
        source,
        "orna.language/1",
        catalogue,
        canonical_origins(),
        Sha256Digest::from_bytes(CANONICAL_STANDARD_DIGEST),
    )
    .unwrap();

    verify_standard_library_snapshot(snapshot).unwrap()
}

fn canonical_origins() -> Vec<DefinitionOrigin> {
    CANONICAL_DECLARATION_FACTS
        .iter()
        .map(|fact| {
            DefinitionOrigin::new(
                canonical_definition_identity(fact.identity),
                canonical_source_origin(*fact),
            )
        })
        .collect()
}

fn canonical_definition_identity(identity: CanonicalDeclarationIdentity) -> DefinitionIdentity {
    match identity {
        CanonicalDeclarationIdentity::Schema(index) => {
            DefinitionIdentity::Schema(SchemaId::from_bytes(CANONICAL_SCHEMA_IDS[index]))
        }
        CanonicalDeclarationIdentity::ValueType(index) => {
            DefinitionIdentity::ValueType(TypeId::from_bytes(CANONICAL_TYPE_IDS[index]))
        }
        CanonicalDeclarationIdentity::TypeBinding(index) => DefinitionIdentity::TypeBinding(
            orna_core::TypeBindingId::from_bytes(CANONICAL_BINDING_IDS[index]),
        ),
    }
}

fn canonical_source_origin(fact: CanonicalDeclarationFact) -> SourceOrigin {
    SourceOrigin::new(
        SourceUnitId::from_bytes(CANONICAL_RESERVED_ID),
        fact.byte_start,
        fact.byte_end,
    )
    .unwrap()
}

fn semantic_name_from_dotted(name: &str) -> QualifiedSemanticName {
    QualifiedSemanticName::new(name.split('.')).unwrap()
}

fn semantic_name<const COUNT: usize>(parts: [&str; COUNT]) -> QualifiedSemanticName {
    QualifiedSemanticName::new(parts).unwrap()
}

fn assert_standard_context_error(
    result: Result<StandardApplicationCheckContext<'_>, StandardApplicationContextError>,
    expected: StandardApplicationContextError,
    message: String,
) {
    assert!(result.is_err());
    let Err(error) = result else {
        return;
    };
    assert_eq!(error, expected);
    assert_eq!(error.clone(), error);
    assert_eq!(error.to_string(), message);
    assert!(std::error::Error::source(&error).is_none());
}

fn standard_origin(
    identity: DefinitionIdentity,
    byte_start: u32,
    byte_end: u32,
) -> DefinitionOrigin {
    DefinitionOrigin::new(
        identity,
        SourceOrigin::new(
            SourceUnitId::from_bytes(CANONICAL_RESERVED_ID),
            byte_start,
            byte_end,
        )
        .unwrap(),
    )
}

fn source_origin(byte_start: u32, byte_end: u32) -> SourceOrigin {
    SourceOrigin::new(
        SourceUnitId::from_bytes(CANONICAL_RESERVED_ID),
        byte_start,
        byte_end,
    )
    .unwrap()
}

fn verified_empty_catalogue_fixture(
    units: &[(&str, &str)],
    standard_digest: [u8; 32],
) -> orna_core::revision::VerifiedStandardLibrarySnapshot {
    let stored_units = units
        .iter()
        .enumerate()
        .map(|(ordinal, (logical_path, content))| {
            let source_unit_id = if units.len() == 1 {
                SourceUnitId::from_bytes(CANONICAL_RESERVED_ID)
            } else {
                SourceUnitId::from_bytes([u8::try_from(ordinal + 4).unwrap(); 16])
            };
            StoredSourceUnit::new(
                source_unit_id,
                u32::try_from(ordinal).unwrap(),
                *logical_path,
                *content,
                source_unit_content_digest(content).unwrap(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    let bundle_hash = source_bundle_digest(&stored_units).unwrap();
    let source = StoredSourceRevision::new(
        SourceBundleId::from_bytes([5; 16]),
        SourceRevisionId::from_bytes([6; 16]),
        None,
        stored_units,
        bundle_hash,
        source_revision_record_digest(SourceBundleId::from_bytes([5; 16]), None, bundle_hash)
            .unwrap(),
    )
    .unwrap();
    let catalogue = CatalogueSnapshot::new_with_types(
        CatalogueRevisionId::from_bytes([8; 16]),
        vec![],
        vec![],
        vec![],
        vec![],
    )
    .unwrap();
    let snapshot = StandardLibrarySnapshot::new(
        StandardLibraryRevisionId::from_bytes([7; 16]),
        StandardLibraryDigestVersion::Version1,
        source,
        "orna.language/1",
        catalogue,
        vec![],
        Sha256Digest::from_bytes(standard_digest),
    )
    .unwrap();

    verify_standard_library_snapshot(snapshot).unwrap()
}

#[test]
fn retains_exact_source_and_syntax_text_in_bundle_order() {
    let first_source = "-- customer source\r\nCREATE SCHEMA crm;  \r\n";
    let second_source = "/* task source */\nCREATE SCHEMA tasks;\n";
    let bundle = SourceBundle::new([
        SourceUnit::new("crm/schema.orna", first_source),
        SourceUnit::new("tasks/schema.orna", second_source),
    ])
    .unwrap();

    let report = parse_bundle(&bundle);

    assert!(report.diagnostics().is_empty());
    assert_eq!(report.units().len(), 2);
    assert_eq!(report.units()[0].logical_path(), "crm/schema.orna");
    assert_eq!(report.units()[0].source_text(), first_source);
    assert_eq!(report.units()[0].syntax_text(), first_source);
    assert_eq!(report.units()[1].logical_path(), "tasks/schema.orna");
    assert_eq!(report.units()[1].source_text(), second_source);
    assert_eq!(report.units()[1].syntax_text(), second_source);
}

#[test]
fn parses_later_units_after_an_earlier_syntax_error() {
    let bundle = SourceBundle::new([
        SourceUnit::new("broken.orna", "CREATE SCHEMA crm.;"),
        SourceUnit::new("valid.orna", "-- retained\nCREATE SCHEMA tasks;"),
    ])
    .unwrap();

    let report = parse_bundle(&bundle);

    assert_eq!(report.units().len(), 2);
    assert_eq!(
        report.units()[1].syntax_text(),
        "-- retained\nCREATE SCHEMA tasks;"
    );
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::UnexpectedToken
    );
}

#[test]
fn maps_syntax_codes_and_retains_owned_paths_and_byte_spans() {
    let bundle = SourceBundle::new([
        SourceUnit::new("syntax.orna", "CREATE SCHEMA crm.;"),
        SourceUnit::new("comment.orna", "/* unfinished"),
    ])
    .unwrap();

    let report = parse_bundle(&bundle);

    assert_eq!(report.diagnostics().len(), 2);
    assert_eq!(
        report.diagnostics()[0].code(),
        DiagnosticCode::UnexpectedToken
    );
    assert_eq!(report.diagnostics()[0].code().as_str(), "ORNA0001");
    assert_eq!(
        report.diagnostics()[0].location().logical_path(),
        "syntax.orna"
    );
    assert_eq!(report.diagnostics()[0].location().span().start(), 18);
    assert_eq!(report.diagnostics()[0].location().span().end(), 19);
    assert_eq!(
        report.diagnostics()[1].code(),
        DiagnosticCode::UnterminatedSourceConstruct
    );
    assert_eq!(report.diagnostics()[1].code().as_str(), "ORNA0002");
    assert_eq!(
        report.diagnostics()[1].location().logical_path(),
        "comment.orna"
    );
    assert_eq!(report.diagnostics()[1].location().span().start(), 0);
    assert_eq!(report.diagnostics()[1].location().span().end(), 13);
}
