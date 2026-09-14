//! Conversion-ready immutable source catalogue candidates.
//!
//! This module exposes the compiler's already validated candidate material to
//! a later runtime adapter. It deliberately carries core catalogue identities,
//! never runtime `sys.Object` identities or `ObjectRef` values.

use std::{error::Error, fmt};

use orna_core::{
    FunctionId, FunctionRevisionId, TypeId,
    canonical_hash::artifact_payload_digest,
    catalogue::{CatalogueSnapshot, FunctionDefinition, FunctionReturn},
    catalogue_diff::{CatalogueSemanticDiff, catalogue_diff},
    revision::{
        ActiveDatabaseRevision, DefinitionOrigin, DefinitionReference, DeployableRevision,
        ExecutableArtifact, ExpressionArtifact, FunctionRevisionRecord, RevisionPair, Sha256Digest,
        StoredSourceRevision,
    },
    types::ResolvedType,
};

use crate::{
    CheckReport, PrepareError, PrepareStandardApplicationError, StandardApplicationCheckReport,
    prepare, prepare_standard_application,
};

/// One signature slot whose exact resolved type may be projected by a runtime
/// invocation adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignatureSlot {
    /// A function parameter identified by its declaration ordinal.
    Parameter(u32),
    /// The function's single return slot.
    Result,
}

/// A function result shape not representable by the current runtime catalogue
/// admission boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedReturnShape {
    /// The function returns a stream rather than one value.
    Stream,
    /// The function returns an ordered row shape.
    Rows,
}

/// Failure while producing a runtime-convertible view of a core candidate.
#[derive(Debug)]
#[non_exhaustive]
pub enum ResolvedSourceCatalogueError {
    /// The existing compiler candidate pipeline rejected the source.
    Preparation(PrepareError),
    /// Standard-authorized preparation rejected the source or its authority.
    StandardPreparation(PrepareStandardApplicationError),
    /// A function has a result shape outside the current runtime boundary.
    UnsupportedReturn {
        /// The core function identity.
        function: FunctionId,
        /// The exact unsupported result shape.
        shape: UnsupportedReturnShape,
    },
    /// A signature type has no exact runtime catalogue declaration form.
    UnsupportedType {
        /// The core function identity.
        function: FunctionId,
        /// The affected signature slot.
        slot: SignatureSlot,
        /// The exact resolved type that was rejected.
        resolved_type: ResolvedType,
    },
    /// A nominal type identity was not present in the candidate or its pinned
    /// standard catalogue.
    MissingType {
        /// The core function identity.
        function: FunctionId,
        /// The affected signature slot.
        slot: SignatureSlot,
        /// The unresolved core type identity.
        type_id: TypeId,
        /// The exact resolved type that referred to it.
        resolved_type: ResolvedType,
    },
    /// A candidate function has no immutable revision record for its current
    /// revision, so it cannot be handed to a runtime admission boundary.
    MissingFunctionRevision {
        /// The candidate function identity.
        function: FunctionId,
        /// The revision named by the candidate function definition.
        revision: FunctionRevisionId,
    },
    /// A retained artifact's payload does not match its immutable digest.
    InvalidFunctionArtifactDigest {
        /// The candidate function identity.
        function: FunctionId,
        /// The immutable function revision identity.
        revision: FunctionRevisionId,
    },
}

impl fmt::Display for ResolvedSourceCatalogueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Preparation(error) => error.fmt(formatter),
            Self::StandardPreparation(error) => error.fmt(formatter),
            Self::UnsupportedReturn { function, shape } => {
                write!(
                    formatter,
                    "function {function} has unsupported return shape {shape:?}"
                )
            }
            Self::UnsupportedType {
                function,
                slot,
                resolved_type,
            } => write!(
                formatter,
                "function {function} has unsupported {slot:?} type {resolved_type:?}"
            ),
            Self::MissingType {
                function,
                slot,
                type_id,
                resolved_type,
            } => write!(
                formatter,
                "function {function} has missing {slot:?} type {resolved_type:?} ({type_id})"
            ),
            Self::MissingFunctionRevision { function, revision } => write!(
                formatter,
                "function {function} has no immutable record for revision {revision}"
            ),
            Self::InvalidFunctionArtifactDigest { function, revision } => write!(
                formatter,
                "function {function} has an invalid artifact digest for revision {revision}"
            ),
        }
    }
}

impl Error for ResolvedSourceCatalogueError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Preparation(error) => Some(error),
            Self::StandardPreparation(error) => Some(error),
            Self::UnsupportedReturn { .. }
            | Self::UnsupportedType { .. }
            | Self::MissingType { .. }
            | Self::MissingFunctionRevision { .. }
            | Self::InvalidFunctionArtifactDigest { .. } => None,
        }
    }
}

/// One immutable compiler-side function handoff entry.
///
/// The definition supplies the exact resolved signature and execution
/// contract. The revision supplies the immutable executable artifact and its
/// digest-bound revision metadata. References are the checked dependency and
/// effect evidence for that exact function revision; this type does not
/// execute or reinterpret the artifact.
#[derive(Clone, Debug)]
struct FunctionArtifactEntry {
    definition: FunctionDefinition,
    revision: FunctionRevisionRecord,
    references: Vec<DefinitionReference>,
}

impl From<PrepareError> for ResolvedSourceCatalogueError {
    fn from(error: PrepareError) -> Self {
        Self::Preparation(error)
    }
}

impl From<PrepareStandardApplicationError> for ResolvedSourceCatalogueError {
    fn from(error: PrepareStandardApplicationError) -> Self {
        Self::StandardPreparation(error)
    }
}

/// A complete immutable source candidate and its identity-preserving change
/// set relative to the active core catalogue.
///
/// The candidate contains core `TypeId`/`FunctionId` identities and compiled
/// executable facts only. It contains no runtime object identifiers and does
/// not construct `ObjectRef`, `TypeRef`, or `FunctionRef` values.
#[derive(Clone, Debug)]
pub struct ResolvedSourceCatalogue {
    base: CatalogueSnapshot,
    candidate: DeployableRevision,
    diff: CatalogueSemanticDiff,
    function_artifacts: Vec<FunctionArtifactEntry>,
}

impl ResolvedSourceCatalogue {
    /// Returns the active immutable catalogue used as the candidate parent.
    pub fn base(&self) -> &CatalogueSnapshot {
        &self.base
    }

    /// Returns the complete immutable source revision.
    pub fn source(&self) -> &StoredSourceRevision {
        self.candidate.source()
    }

    /// Returns the complete immutable candidate catalogue.
    pub fn candidate(&self) -> &CatalogueSnapshot {
        self.candidate.candidate()
    }

    /// Returns the identity-preserving diff from `base` to `candidate`.
    pub fn diff(&self) -> &CatalogueSemanticDiff {
        &self.diff
    }

    /// Returns the expected active source/catalogue pair.
    pub const fn expected_base(&self) -> RevisionPair {
        self.candidate.expected_base()
    }

    /// Returns the source/catalogue pair produced by this candidate.
    pub const fn candidate_pair(&self) -> RevisionPair {
        self.candidate.candidate_pair()
    }

    /// Returns the canonical candidate catalogue hash.
    pub const fn catalogue_hash(&self) -> Sha256Digest {
        self.candidate.catalogue_hash()
    }

    /// Returns declaration-origin facts for the candidate.
    pub fn origins(&self) -> &[DefinitionOrigin] {
        self.candidate.origins()
    }

    /// Returns compiled expression artifacts for the candidate.
    pub fn expressions(&self) -> &[ExpressionArtifact] {
        self.candidate.expressions()
    }

    /// Returns function revisions newly installed by this candidate.
    pub fn new_function_revisions(&self) -> &[FunctionRevisionRecord] {
        self.candidate.new_function_revisions()
    }

    /// Returns complete current function-revision evidence when present.
    pub fn current_function_revisions(&self) -> Option<&[FunctionRevisionRecord]> {
        self.candidate.current_function_revisions()
    }

    /// Returns resolved references for the candidate's current functions.
    pub fn references(&self) -> &[DefinitionReference] {
        self.candidate.references()
    }

    /// Returns each candidate function with its exact current revision,
    /// executable artifact handle, and checked references.
    ///
    /// The returned entries are immutable views assembled during
    /// materialization. A missing function/revision binding is rejected before
    /// a catalogue is returned.
    pub fn function_artifacts(
        &self,
    ) -> impl Iterator<
        Item = (
            &FunctionDefinition,
            &FunctionRevisionRecord,
            &ExecutableArtifact,
            &[DefinitionReference],
        ),
    > {
        self.function_artifacts.iter().map(|entry| {
            (
                &entry.definition,
                &entry.revision,
                entry.revision.artifact(),
                entry.references.as_slice(),
            )
        })
    }
}

/// Materializes a complete core candidate from a successful compiler check.
///
/// This is a pure operation: it allocates only core candidate identities and
/// does not mutate storage. The active revision supplies the exact parent
/// catalogue, so the returned diff is identity-based rather than a name-based
/// guess. Signature validation is fail-closed for the current runtime
/// invocation projection.
pub fn materialize_resolved_source_catalogue(
    report: &CheckReport,
    expected_base: RevisionPair,
    active: &ActiveDatabaseRevision,
) -> Result<ResolvedSourceCatalogue, ResolvedSourceCatalogueError> {
    let candidate = prepare(report, expected_base, active)?;
    materialize_prepared_source_catalogue(candidate, active)
}

/// Materializes a complete core candidate from a successful standard-authorized
/// compiler check.
///
/// Standard application checking carries the pinned standard catalogue and
/// library digest that cannot be represented by the legacy [`CheckReport`].
/// This additive entry point preserves that authority through
/// [`prepare_standard_application`] and shares the same runtime projection
/// boundary as the legacy entry point.
pub fn materialize_standard_resolved_source_catalogue(
    report: &StandardApplicationCheckReport,
    expected_base: RevisionPair,
    active: &ActiveDatabaseRevision,
) -> Result<ResolvedSourceCatalogue, ResolvedSourceCatalogueError> {
    let candidate = prepare_standard_application(report, expected_base, active)?;
    materialize_prepared_source_catalogue(candidate, active)
}

fn materialize_prepared_source_catalogue(
    candidate: DeployableRevision,
    active: &ActiveDatabaseRevision,
) -> Result<ResolvedSourceCatalogue, ResolvedSourceCatalogueError> {
    validate_invocation_projection(&candidate)?;
    let function_artifacts = materialize_function_artifacts(&candidate, active)?;
    let diff = catalogue_diff(active.catalogue(), candidate.candidate());
    Ok(ResolvedSourceCatalogue {
        base: active.catalogue().clone(),
        candidate,
        diff,
        function_artifacts,
    })
}

fn materialize_function_artifacts(
    candidate: &DeployableRevision,
    active: &ActiveDatabaseRevision,
) -> Result<Vec<FunctionArtifactEntry>, ResolvedSourceCatalogueError> {
    candidate
        .candidate()
        .functions()
        .iter()
        .map(|definition| {
            let revision_id = definition.current_revision();
            let (revision, candidate_revision) = find_function_revision(
                definition,
                candidate.current_function_revisions(),
                candidate.new_function_revisions(),
                active.function_revisions(),
            )?;
            validate_function_artifact_digest(
                definition.id(),
                revision_id,
                revision.artifact().payload(),
                revision.artifact().content_hash(),
            )?;

            let references = select_function_references(
                candidate.references(),
                active.references(),
                definition.id(),
                revision_id,
                candidate_revision,
            );
            Ok(FunctionArtifactEntry {
                definition: definition.clone(),
                revision: revision.clone(),
                references,
            })
        })
        .collect()
}

fn validate_function_artifact_digest(
    function: FunctionId,
    revision: FunctionRevisionId,
    payload: &[u8],
    content_hash: Sha256Digest,
) -> Result<(), ResolvedSourceCatalogueError> {
    if artifact_payload_digest(payload).ok() != Some(content_hash) {
        return Err(
            ResolvedSourceCatalogueError::InvalidFunctionArtifactDigest { function, revision },
        );
    }
    Ok(())
}

fn find_function_revision<'a>(
    definition: &FunctionDefinition,
    candidate_current: Option<&'a [FunctionRevisionRecord]>,
    candidate_new: &'a [FunctionRevisionRecord],
    active: &'a [FunctionRevisionRecord],
) -> Result<(&'a FunctionRevisionRecord, bool), ResolvedSourceCatalogueError> {
    let revision_id = definition.current_revision();
    let candidate_revision = candidate_current
        .into_iter()
        .flatten()
        .chain(candidate_new)
        .find(|revision| revision.function() == definition.id() && revision.id() == revision_id);
    if let Some(revision) = candidate_revision {
        return Ok((revision, true));
    }
    active
        .iter()
        .find(|revision| revision.function() == definition.id() && revision.id() == revision_id)
        .map(|revision| (revision, false))
        .ok_or(ResolvedSourceCatalogueError::MissingFunctionRevision {
            function: definition.id(),
            revision: revision_id,
        })
}

fn select_function_references(
    candidate: &[DefinitionReference],
    active: &[DefinitionReference],
    function: FunctionId,
    revision: FunctionRevisionId,
    candidate_revision: bool,
) -> Vec<DefinitionReference> {
    let candidate_references = candidate
        .iter()
        .filter(|reference| {
            reference.source_function() == function && reference.source_revision() == revision
        })
        .cloned()
        .collect::<Vec<_>>();
    if candidate_revision || !candidate_references.is_empty() {
        return candidate_references;
    }
    active
        .iter()
        .filter(|reference| {
            reference.source_function() == function && reference.source_revision() == revision
        })
        .cloned()
        .collect()
}

fn validate_invocation_projection(
    candidate: &DeployableRevision,
) -> Result<(), ResolvedSourceCatalogueError> {
    for function in candidate.candidate().functions() {
        match function.return_type() {
            FunctionReturn::Single(resolved_type) => validate_signature_type(
                candidate,
                function.id(),
                SignatureSlot::Result,
                *resolved_type,
            )?,
            FunctionReturn::Stream(_) => {
                return Err(ResolvedSourceCatalogueError::UnsupportedReturn {
                    function: function.id(),
                    shape: UnsupportedReturnShape::Stream,
                });
            }
            FunctionReturn::Rows(_) => {
                return Err(ResolvedSourceCatalogueError::UnsupportedReturn {
                    function: function.id(),
                    shape: UnsupportedReturnShape::Rows,
                });
            }
        }

        for parameter in function.parameters() {
            validate_signature_type(
                candidate,
                function.id(),
                SignatureSlot::Parameter(parameter.ordinal()),
                parameter.resolved_type(),
            )?;
        }
    }
    Ok(())
}

fn validate_signature_type(
    candidate: &DeployableRevision,
    function: FunctionId,
    slot: SignatureSlot,
    resolved_type: ResolvedType,
) -> Result<(), ResolvedSourceCatalogueError> {
    let has_candidate_object = |type_id| candidate.candidate().object_type_by_id(type_id).is_some();
    let has_standard_object = |type_id| {
        candidate
            .catalogue_hash_context()
            .standard()
            .is_some_and(|standard| standard.catalogue().object_type_by_id(type_id).is_some())
    };
    let has_candidate_value = |type_id| candidate.candidate().value_type_by_id(type_id).is_some();
    let has_standard_value = |type_id| {
        candidate
            .catalogue_hash_context()
            .standard()
            .is_some_and(|standard| standard.catalogue().value_type_by_id(type_id).is_some())
    };

    match resolved_type {
        ResolvedType::Scalar(_) => Err(ResolvedSourceCatalogueError::UnsupportedType {
            function,
            slot,
            resolved_type,
        }),
        ResolvedType::Named(type_id) => {
            if has_candidate_object(type_id) || has_standard_object(type_id) {
                Ok(())
            } else {
                Err(ResolvedSourceCatalogueError::MissingType {
                    function,
                    slot,
                    type_id,
                    resolved_type,
                })
            }
        }
        ResolvedType::Value(type_id) => {
            if has_candidate_value(type_id) || has_standard_value(type_id) {
                Ok(())
            } else {
                Err(ResolvedSourceCatalogueError::MissingType {
                    function,
                    slot,
                    type_id,
                    resolved_type,
                })
            }
        }
        ResolvedType::Reference { target } => {
            if has_candidate_object(target) || has_standard_object(target) {
                Err(ResolvedSourceCatalogueError::UnsupportedType {
                    function,
                    slot,
                    resolved_type,
                })
            } else {
                Err(ResolvedSourceCatalogueError::MissingType {
                    function,
                    slot,
                    type_id: target,
                    resolved_type,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_core::{
        CatalogueRevisionId, FunctionId, FunctionRevisionId, SourceBundleId, SourceRevisionId,
        SourceUnitId,
        canonical_hash::{
            artifact_payload_digest, catalogue_digest, source_bundle_digest,
            source_revision_record_digest,
        },
        revision::{
            ActiveDatabaseRevision, DefinitionIdentity, DefinitionReferenceKind,
            DefinitionReferenceTarget, DeployableRevision, ExecutableArtifact,
            ExecutableArtifactKind, FunctionRevisionRecord, RevisionPair, SourceOrigin,
            StoredSourceRevision, StoredSourceUnit,
        },
        source::{SourceBundle, SourceUnit},
        types::StandardScalar,
    };

    #[test]
    fn materializes_function_artifact_entry_bound_to_catalogue_revision_and_digest() {
        let active = empty_active();
        let source = SourceBundle::new([SourceUnit::new(
            "tasks.orna",
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.echo() RETURNS BOOLEAN RETURN TRUE;",
        )])
        .unwrap();
        let report = crate::check(&source, active.catalogue());
        assert!(
            report.diagnostics().is_empty(),
            "{:?}",
            report.diagnostics()
        );

        let candidate = crate::prepare(&report, active.pair(), &active).unwrap();
        let entries = materialize_function_artifacts(&candidate, &active).unwrap();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];

        assert_eq!(entry.definition.id(), entry.revision.function());
        assert_eq!(entry.definition.current_revision(), entry.revision.id());
        assert_eq!(
            artifact_payload_digest(entry.revision.artifact().payload()).unwrap(),
            entry.revision.artifact().content_hash()
        );
        assert!(entry.references.is_empty());
    }

    #[test]
    fn public_materialization_rejects_unrepresentable_scalar_before_handoff() {
        let active = empty_active();
        let source = SourceBundle::new([SourceUnit::new(
            "tasks.orna",
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.echo() RETURNS BOOLEAN RETURN TRUE;",
        )])
        .unwrap();
        let report = crate::check(&source, active.catalogue());
        assert!(
            report.diagnostics().is_empty(),
            "{:?}",
            report.diagnostics()
        );

        assert!(matches!(
            materialize_resolved_source_catalogue(&report, active.pair(), &active),
            Err(ResolvedSourceCatalogueError::UnsupportedType {
                slot: SignatureSlot::Result,
                resolved_type: ResolvedType::Scalar(StandardScalar::Boolean),
                ..
            })
        ));
    }

    #[test]
    fn candidate_reference_evidence_is_selected_without_active_duplicates() {
        let function = FunctionId::from_bytes([0x31; 16]);
        let revision = FunctionRevisionId::from_bytes([0x32; 16]);
        let origin = SourceOrigin::new(SourceUnitId::new(), 0, 1).unwrap();
        let candidate = vec![
            DefinitionReference::new(
                function,
                revision,
                0,
                DefinitionReferenceTarget::Function(FunctionId::from_bytes([0x41; 16])),
                DefinitionReferenceKind::FunctionCall,
                origin,
            ),
            DefinitionReference::new(
                function,
                revision,
                1,
                DefinitionReferenceTarget::Function(FunctionId::from_bytes([0x42; 16])),
                DefinitionReferenceKind::FunctionCall,
                origin,
            ),
        ];
        let active = vec![DefinitionReference::new(
            function,
            revision,
            0,
            DefinitionReferenceTarget::Function(FunctionId::from_bytes([0x51; 16])),
            DefinitionReferenceKind::FunctionCall,
            origin,
        )];

        let selected = select_function_references(&candidate, &active, function, revision, true);
        assert_eq!(selected.len(), 2);
        assert_eq!(
            selected
                .iter()
                .map(DefinitionReference::ordinal)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert_eq!(selected[0].target(), candidate[0].target());
        assert_eq!(selected[1].target(), candidate[1].target());

        let fallback = select_function_references(&[], &active, function, revision, false);
        assert_eq!(fallback, active);
    }

    #[test]
    fn missing_function_revision_fails_closed_before_public_handoff() {
        let active = empty_active();
        let revision = FunctionRevisionId::from_bytes([0x82; 16]);
        let candidate = candidate_with_function(
            &active,
            revision,
            vec![valid_function_revision(
                revision,
                artifact_payload_digest(&[0x01, 0x02]).unwrap(),
            )],
        );
        let function = &candidate.candidate().functions()[0];

        assert!(matches!(
            find_function_revision(function, None, &[], &[]),
            Err(ResolvedSourceCatalogueError::MissingFunctionRevision {
                revision: actual,
                ..
            }) if actual == revision
        ));
    }

    #[test]
    fn invalid_function_artifact_digest_fails_closed_before_public_handoff() {
        let active = empty_active();
        let revision = FunctionRevisionId::from_bytes([0x83; 16]);
        let record =
            valid_function_revision(revision, artifact_payload_digest(&[0x01, 0x02]).unwrap());
        let candidate = candidate_with_function(&active, revision, vec![record]);
        let record = &candidate.new_function_revisions()[0];

        assert!(matches!(
            validate_function_artifact_digest(
                record.function(),
                record.id(),
                record.artifact().payload(),
                Sha256Digest::from_bytes([0xff; 32]),
            ),
            Err(ResolvedSourceCatalogueError::InvalidFunctionArtifactDigest {
                revision: actual,
                ..
            }) if actual == revision
        ));
    }

    fn valid_function_revision(
        revision: FunctionRevisionId,
        content_hash: Sha256Digest,
    ) -> FunctionRevisionRecord {
        FunctionRevisionRecord::new(
            FunctionId::from_bytes([0x81; 16]),
            revision,
            1,
            SourceOrigin::new(SourceUnitId::from_bytes([0x84; 16]), 0, 18).unwrap(),
            Sha256Digest::from_bytes([0x91; 32]),
            Sha256Digest::from_bytes([0x92; 32]),
            "orna.language/1",
            ExecutableArtifact::new(
                ExecutableArtifactKind::Client,
                "test.artifact",
                1,
                vec![0x01, 0x02],
                content_hash,
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn candidate_with_function(
        active: &ActiveDatabaseRevision,
        revision: FunctionRevisionId,
        revisions: Vec<FunctionRevisionRecord>,
    ) -> DeployableRevision {
        let source_unit_id = SourceUnitId::from_bytes([0x84; 16]);
        let source_text = "CREATE SCHEMA app;";
        let source_unit = StoredSourceUnit::new(
            source_unit_id,
            0,
            "candidate.orna",
            source_text,
            orna_core::canonical_hash::source_unit_content_digest(source_text).unwrap(),
        )
        .unwrap();
        let bundle_hash = source_bundle_digest(std::slice::from_ref(&source_unit)).unwrap();
        let source = StoredSourceRevision::new(
            SourceBundleId::from_bytes([0x85; 16]),
            SourceRevisionId::from_bytes([0x86; 16]),
            Some(active.source().id()),
            vec![source_unit],
            bundle_hash,
            source_revision_record_digest(
                SourceBundleId::from_bytes([0x85; 16]),
                Some(active.source().id()),
                bundle_hash,
            )
            .unwrap(),
        )
        .unwrap();
        let schema = orna_core::catalogue::SchemaDefinition::new(
            orna_core::SchemaId::from_bytes([0x87; 16]),
            orna_core::catalogue::QualifiedSemanticName::new(["app"]).unwrap(),
        );
        let object_type_id = TypeId::from_bytes([0x88; 16]);
        let function_id = FunctionId::from_bytes([0x81; 16]);
        let function = FunctionDefinition::new(
            function_id,
            orna_core::catalogue::QualifiedSemanticName::new(["app", "echo"]).unwrap(),
            orna_core::catalogue::FunctionDomain::Client,
            Vec::new(),
            FunctionReturn::Single(ResolvedType::Named(object_type_id)),
            revision,
            orna_core::catalogue::FunctionSecurity::Invoker,
            None,
            orna_core::catalogue::FunctionVolatility::Immutable,
        );
        let object = orna_core::catalogue::ObjectTypeDefinition::new(
            object_type_id,
            orna_core::catalogue::QualifiedSemanticName::new(["app", "item"]).unwrap(),
            Vec::new(),
        );
        let catalogue = CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::from_bytes([0x89; 16]),
            vec![schema],
            vec![object],
            vec![function.clone()],
        )
        .unwrap();
        let origin = SourceOrigin::new(source_unit_id, 0, source_text.len() as u32).unwrap();
        let origins = vec![
            DefinitionOrigin::new(
                DefinitionIdentity::Schema(orna_core::SchemaId::from_bytes([0x87; 16])),
                origin,
            ),
            DefinitionOrigin::new(DefinitionIdentity::ObjectType(object_type_id), origin),
            DefinitionOrigin::new(DefinitionIdentity::Function(function_id), origin),
        ];
        let revisions = revisions
            .into_iter()
            .map(|revision_record| {
                let semantic_hash = orna_core::canonical_hash::function_semantic_digest(
                    &function,
                    revision_record.language_version(),
                    revision_record.artifact(),
                    &[],
                    &[],
                )
                .unwrap();
                FunctionRevisionRecord::new(
                    revision_record.function(),
                    revision_record.id(),
                    revision_record.revision_number(),
                    revision_record.declaration_origin(),
                    revision_record.declaration_content_hash(),
                    semantic_hash,
                    revision_record.language_version(),
                    revision_record.artifact().clone(),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let catalogue_hash = catalogue_digest(&catalogue, &revisions, &[], &origins, &[]).unwrap();
        DeployableRevision::new(
            active.pair(),
            source,
            active.catalogue().revision(),
            catalogue,
            catalogue_hash,
            origins,
            Vec::new(),
            revisions,
            Vec::new(),
        )
        .unwrap()
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
}
