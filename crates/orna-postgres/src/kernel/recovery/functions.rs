//! Recovery of executable function catalogue state.

// Result APIs intentionally preserve the accepted public `PostgresKernelError` layout.
#![allow(clippy::result_large_err)]
#[path = "functions/catalogue.rs"]
mod catalogue;
#[path = "functions/references.rs"]
mod references;
#[path = "functions/revisions.rs"]
mod revisions;

use catalogue::require_catalogue;
pub(super) use catalogue::{RecoveredFunction, load_catalogue_functions};
#[cfg(test)]
use references::{SUPPORTED_REFERENCE_KINDS, decode_reference_kind, reference_kind_matches_target};
pub(super) use references::{load_references, validate_reference_sources};
pub(super) use revisions::{
    RecoveredFunctionState, load_catalogue_current_revisions, load_function_state,
};
use std::collections::{BTreeMap, BTreeSet};

use orna_core::{
    CatalogueRevisionId, ExpressionId, FunctionId, FunctionRevisionId, ParameterId, SchemaId,
    SourceBundleId, SourceRevisionId, SourceUnitId, StandardLibraryRevisionId, TypeId,
    canonical_hash::{
        artifact_payload_digest, function_declaration_digest, source_bundle_digest,
        source_revision_digest,
    },
    catalogue::{
        FunctionDefinition, FunctionDomain, FunctionReturn, FunctionReturnColumnDefinition,
        FunctionSecurity, FunctionTransaction, FunctionVolatility, ParameterDefinition,
        QualifiedSemanticName,
    },
    revision::{
        CatalogueHashContext, CatalogueHashVersion, DefinitionIdentity, DefinitionOrigin,
        DefinitionReference, DefinitionReferenceKind, DefinitionReferenceTarget,
        ExecutableArtifact, ExecutableArtifactKind, FunctionRevisionRecord,
        FunctionSemanticHashVersion, Sha256Digest, SourceOrigin, StoredSourceRevision,
    },
    types::ResolvedType,
};
use tokio_postgres::{Row, Transaction};

#[cfg(test)]
use orna_core::types::StandardScalar;

use crate::{
    PostgresKernelError,
    decode::{
        DurableRecord, digest_bytes, exact_enum, identity_bytes, optional_identity_bytes,
        u32_from_i64, u64_from_i64,
    },
    is_sealed_inspect_type_id,
};

use super::{
    LegacyResolvedTypeTupleMember, ResolvedTypeTuple, decode_catalogue_hash_version,
    decode_durable_version, decode_legacy_resolved_type_tuple,
    decode_legacy_resolved_type_tuple_kind, decode_origin, decode_resolved_type_tuple,
    load_source_units, require_hash_contract,
};

const FUNCTION_RELATION: &str = "_orna_kernel.catalogue_functions";
const PARAMETER_RELATION: &str = "_orna_kernel.catalogue_function_parameters";
const RETURN_RELATION: &str = "_orna_kernel.catalogue_function_return_columns";
const REVISION_RELATION: &str = "_orna_kernel.function_revisions";
const ARTIFACT_RELATION: &str = "_orna_kernel.function_artifacts";
const REFERENCE_RELATION: &str = "_orna_kernel.definition_references";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reference_kind_decoder_maps_all_supported_spellings_exactly() {
        let record = DurableRecord::new(REFERENCE_RELATION, "test");
        let expected = [
            ("function_call", DefinitionReferenceKind::FunctionCall),
            ("named_type", DefinitionReferenceKind::NamedType),
            ("object_reference", DefinitionReferenceKind::ObjectReference),
            ("parameter_read", DefinitionReferenceKind::ParameterRead),
            ("query_object", DefinitionReferenceKind::QueryObject),
            ("query_field", DefinitionReferenceKind::QueryField),
            ("expression", DefinitionReferenceKind::Expression),
            ("write_object", DefinitionReferenceKind::WriteObject),
            ("write_field", DefinitionReferenceKind::WriteField),
        ];

        assert_eq!(SUPPORTED_REFERENCE_KINDS, expected.as_slice());
        for (name, kind) in expected {
            assert_eq!(decode_reference_kind(name, &record).unwrap(), kind);
        }
    }

    #[test]
    fn reference_kind_decoder_rejects_unknown_spellings() {
        let record = DurableRecord::new(REFERENCE_RELATION, "test");

        assert!(decode_reference_kind("write_Object", &record).is_err());
        assert!(decode_reference_kind("insert", &record).is_err());
    }

    #[test]
    fn write_reference_kinds_require_their_exact_targets() {
        let object = TypeId::from_bytes([1; 16]);
        let field = orna_core::FieldId::from_bytes([2; 16]);

        assert!(reference_kind_matches_target(
            DefinitionReferenceKind::WriteObject,
            DefinitionReferenceTarget::ObjectType(object),
        ));
        assert!(reference_kind_matches_target(
            DefinitionReferenceKind::WriteField,
            DefinitionReferenceTarget::Field {
                owner: object,
                field,
            },
        ));
        assert!(!reference_kind_matches_target(
            DefinitionReferenceKind::WriteObject,
            DefinitionReferenceTarget::Field {
                owner: object,
                field,
            },
        ));
        assert!(!reference_kind_matches_target(
            DefinitionReferenceKind::WriteField,
            DefinitionReferenceTarget::ObjectType(object),
        ));
    }

    #[test]
    fn named_type_references_accept_only_value_type_targets_in_the_new_family() {
        let value_type = TypeId::from_bytes([3; 16]);

        assert!(reference_kind_matches_target(
            DefinitionReferenceKind::NamedType,
            DefinitionReferenceTarget::ValueType(value_type),
        ));
        assert!(!reference_kind_matches_target(
            DefinitionReferenceKind::ObjectReference,
            DefinitionReferenceTarget::ValueType(value_type),
        ));
    }

    #[test]
    fn void_scalar_is_reserved_for_single_function_returns() {
        let record = DurableRecord::new(PARAMETER_RELATION, "function=test parameter=test");
        let single_kind = decode_legacy_resolved_type_tuple_kind(
            Some("scalar"),
            &record,
            LegacyResolvedTypeTupleMember::SingleReturn,
        )
        .expect("SINGLE scalar kind");
        let parameter_kind = decode_legacy_resolved_type_tuple_kind(
            Some("scalar"),
            &record,
            LegacyResolvedTypeTupleMember::Parameter,
        )
        .expect("parameter scalar kind");

        assert_eq!(
            decode_legacy_resolved_type_tuple(
                single_kind,
                Some("void"),
                None,
                &record,
                LegacyResolvedTypeTupleMember::SingleReturn,
            )
            .expect("SINGLE return void"),
            ResolvedType::scalar(StandardScalar::Void)
        );
        assert!(matches!(
            decode_legacy_resolved_type_tuple(
                parameter_kind,
                Some("void"),
                None,
                &record,
                LegacyResolvedTypeTupleMember::Parameter,
            ),
            Err(PostgresKernelError::DurableInvariant {
                relation: PARAMETER_RELATION,
                record,
                rule: "void is valid only as a SINGLE function return, never as a parameter or ROWS column",
            }) if record == "function=test parameter=test"
        ));
    }
}
