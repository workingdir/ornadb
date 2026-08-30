// Result APIs intentionally preserve the accepted public `PostgresKernelError` layout.
#![allow(clippy::result_large_err)]
use std::collections::{BTreeMap, BTreeSet, HashSet};

#[path = "recovery/active_revision.rs"]
mod active_revision;
#[path = "recovery/assembly.rs"]
mod assembly;
#[path = "recovery/catalogue_expressions.rs"]
mod catalogue_expressions;
#[path = "recovery/catalogue_fields.rs"]
mod catalogue_fields;
#[path = "recovery/catalogue_types.rs"]
mod catalogue_types;
#[path = "recovery/functions.rs"]
mod functions;
#[path = "recovery/headers.rs"]
mod headers;
#[path = "recovery/revision_history.rs"]
mod revision_history;
#[path = "recovery/sources.rs"]
mod sources;
#[path = "recovery/standard.rs"]
mod standard;

pub(crate) use active_revision::recover_active_revision;
use active_revision::recover_client;
#[cfg(test)]
use assembly::{RecoveredCatalogueSemantics, assemble_catalogue_semantics, validate_function_type};
use assembly::{assemble_revision, load_catalogue_semantics};
use catalogue_expressions::{
    RecoveredExpression, decode_origin, load_expressions, require_catalogue_identity,
};
use catalogue_fields::{
    LegacyResolvedTypeTupleMember, RecoveredField, RecoveredRecordValueField, ResolvedTypeTuple,
    decode_legacy_resolved_type_tuple, decode_legacy_resolved_type_tuple_kind,
    decode_resolved_type_tuple, load_fields, load_record_value_fields,
};
#[cfg(test)]
use catalogue_fields::{RecordValueFieldTypeTuple, decode_record_value_field_descriptor};
use catalogue_types::{
    RecoveredEnumType, RecoveredObjectType, RecoveredRecordValueType, RecoveredSchema,
    load_enum_types, load_object_types, load_record_value_types, load_schemas,
};
use headers::{
    HashAlgorithm, RecoveredRevisionHeader, TextEncoding, catalogue_hash_context_for,
    decode_catalogue_hash_version, decode_durable_version, load_active_catalogue_hash_context,
    load_active_header, require_hash_contract,
};
pub use revision_history::RevisionPairHistoryEntry;
#[cfg(test)]
use revision_history::{decode_revision_pair_values, validate_revision_pair_listing};
use sources::load_source_units;
pub(crate) use standard::load_verified_standard_library;
#[cfg(test)]
use standard::{
    decode_standard_binding_target, recovered_standard_value_definition,
    verify_recovered_standard_snapshot,
};

#[cfg(feature = "test-hooks")]
use orna_core::canonical_hash::{
    verify_standard_library_snapshot as verify_structural_standard_library_snapshot,
    verify_standard_library_v2_snapshot as verify_structural_standard_library_v2_snapshot,
};
use orna_core::{
    CatalogueRevisionId, ExpressionId, FieldId, FunctionId, FunctionRevisionId, ParameterId,
    SchemaId, SourceBundleId, SourceRevisionId, SourceUnitId, StandardLibraryRevisionId,
    TypeBindingId, TypeId,
    canonical_hash::{
        artifact_payload_digest, catalogue_digest_with_context, source_bundle_digest,
        source_revision_digest, source_revision_record_digest, source_unit_content_digest,
    },
    catalogue::{
        CatalogueSnapshot, EnumTypeDefinition, FieldDefinition, FunctionDefinition, FunctionDomain,
        FunctionReturn, FunctionSecurity, FunctionTransaction, FunctionVolatility,
        ObjectTypeDefinition, OnDeleteAction, ParameterDefinition, PreludeTypeName,
        QualifiedSemanticName, RecordValueFieldDefinition, RecordValueTypeDefinition,
        SchemaDefinition, TypeBinding, TypeBindingKind, ValueTypeDefinition, ValueTypeKind,
        ValueTypeMutability, ValueTypePersistence,
    },
    revision::{
        ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
        CatalogueHashContext, CatalogueHashVersion, DefinitionIdentity, DefinitionOrigin,
        DefinitionReference, DefinitionReferenceKind, DefinitionReferenceTarget,
        ExecutableArtifact, ExecutableArtifactKind, ExpressionArtifact, FunctionRevisionRecord,
        FunctionSemanticHashVersion, RevisionPair, Sha256Digest, SourceOrigin, StandardExecutable,
        StandardLibraryDigestVersion, StandardLibrarySnapshot, StoredSourceRevision,
        StoredSourceUnit, VerifiedStandardLibrarySnapshot,
    },
    system::SYS_INSPECT_INVOCATION_TYPE_ID,
    types::{ResolvedType, StandardScalar, TypeDescriptor},
};

use orna_standard::{
    STANDARD_LIBRARY_REVISION_ID, STANDARD_LIBRARY_V2_REVISION_ID, STANDARD_LIBRARY_V3_REVISION_ID,
    STANDARD_LIBRARY_V4_REVISION_ID, STANDARD_LIBRARY_V5_REVISION_ID,
    STANDARD_LIBRARY_V6_REVISION_ID, STANDARD_LIBRARY_V7_REVISION_ID,
    STANDARD_LIBRARY_V8_REVISION_ID, STANDARD_LIBRARY_V9_REVISION_ID,
    verify_standard_library_snapshot, verify_standard_library_v2_snapshot,
    verify_standard_library_v3_snapshot, verify_standard_library_v4_snapshot,
    verify_standard_library_v5_snapshot, verify_standard_library_v6_snapshot,
    verify_standard_library_v7_snapshot, verify_standard_library_v8_snapshot,
    verify_standard_library_v9_snapshot,
};
use tokio_postgres::{Client, IsolationLevel, Row, Transaction};

use crate::{
    PostgresKernel, PostgresKernelError,
    bootstrap::require_current_migrations,
    decode::{
        DurableRecord, digest_bytes, exact_enum, identity_bytes, optional_identity_bytes,
        u32_from_i64, u64_from_i64,
    },
    is_sealed_inspect_type_id,
    physical::{establish_trusted_search_path, verify_physical_catalogue},
};

use self::functions::{
    RecoveredFunctionState, load_catalogue_current_revisions, load_catalogue_functions,
    load_function_state, load_references, validate_reference_sources,
};

const ACTIVE_RELATION: &str = "_orna_kernel.active_revision";
const SOURCE_UNIT_RELATION: &str = "_orna_kernel.source_units";
const SOURCE_REVISION_RELATION: &str = "_orna_kernel.source_revisions";
const CATALOGUE_REVISION_RELATION: &str = "_orna_kernel.catalogue_revisions";

impl PostgresKernel {
    /// Reconstructs and validates the complete active durable database revision.
    ///
    /// This recovery slice supports schemas, object and record value types,
    /// fields, expression artifacts, compiler-deployable functions, immutable
    /// function history, and active definition references. It fails closed on
    /// any semantic, source, hash-chain, or physical-layout state it cannot
    /// prove complete.
    pub async fn recover(&self) -> Result<ActiveDatabaseRevision, PostgresKernelError> {
        let mut session = self.open().await?;
        let recovery_result = recover_client(&mut session.client)
            .await
            .map_err(super::map_recovery_client_error);
        let shutdown_result = session.shutdown_for_source_apply().await;

        match (recovery_result, shutdown_result) {
            (Ok(active), Ok(())) => Ok(active),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }
}

#[cfg(test)]
#[path = "recovery/tests.rs"]
mod tests;
