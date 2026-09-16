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
        ActiveDatabaseRevision, ArtifactCatalogueCompatibility, ArtifactCompatibilityCoordinates,
        ArtifactProvenanceError, ArtifactStandardLibraryCompatibility, DefinitionOrigin,
        DefinitionReference, DeployableRevision, ExecutableArtifact, ExecutableArtifactProvenance,
        ExpressionArtifact, FunctionRevisionRecord, RevisionPair, Sha256Digest,
        StoredSourceRevision,
    },
    types::ResolvedType,
};

use crate::{
    CheckReport, CheckedBundle, CheckedObjectType, PrepareError, PrepareStandardApplicationError,
    StandardApplicationCheckReport, prepare, prepare_standard_application,
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
    /// The compiler could not bind a newly compiled revision to the candidate
    /// catalogue that owns it.
    MissingCandidateFunction {
        /// The immutable function identity retained by the revision.
        function: FunctionId,
        /// The immutable revision identity that could not be handed off.
        revision: FunctionRevisionId,
    },
    /// The checked resolver facts needed for candidate metadata were absent.
    ///
    /// A runtime admission layer must never reconstruct these facts from
    /// source spelling or from runtime catalogue identities.
    MissingCheckedBundle,
    /// Constructing the digest-bound artifact provenance envelope failed.
    ArtifactProvenance(ArtifactProvenanceError),
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
            Self::MissingCandidateFunction { function, revision } => write!(
                formatter,
                "candidate has no function {function} for newly compiled revision {revision}"
            ),
            Self::MissingCheckedBundle => {
                formatter.write_str("candidate has no checked resolver bundle")
            }
            Self::ArtifactProvenance(error) => error.fmt(formatter),
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
            | Self::InvalidFunctionArtifactDigest { .. }
            | Self::MissingCandidateFunction { .. } => None,
            Self::MissingCheckedBundle => None,
            Self::ArtifactProvenance(error) => Some(error),
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

/// One immutable compiler-to-runtime artifact provenance handoff.
///
/// This is produced only for executable revisions compiled in the candidate
/// source snapshot. It preserves the checked definition and references beside
/// the digest-bound provenance envelope, but does not admit or execute the
/// artifact.
#[derive(Clone, Debug)]
pub struct ProvenancedFunctionArtifact {
    definition: FunctionDefinition,
    revision: FunctionRevisionRecord,
    references: Vec<DefinitionReference>,
    provenance: ExecutableArtifactProvenance,
}

impl ProvenancedFunctionArtifact {
    /// Returns the exact candidate definition that owns the artifact.
    pub fn definition(&self) -> &FunctionDefinition {
        &self.definition
    }

    /// Returns the immutable compiled function revision and artifact bytes.
    pub fn revision(&self) -> &FunctionRevisionRecord {
        &self.revision
    }

    /// Returns the checked dependency and effect reference evidence.
    pub fn references(&self) -> &[DefinitionReference] {
        &self.references
    }

    /// Returns the digest-bound source, catalogue, and compatibility evidence.
    pub fn provenance(&self) -> &ExecutableArtifactProvenance {
        &self.provenance
    }
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
    checked_nominal_types: Vec<CheckedObjectType>,
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

    /// Returns checked candidate object/nominal declarations in source order.
    ///
    /// These are compiler identities and checked source facts only. They are
    /// not runtime object references and do not allocate or derive runtime
    /// `ObjectId` values. The current object-type grammar has no separate
    /// visibility field; consumers must therefore not infer private-member
    /// authority from this view.
    pub fn checked_nominal_types(&self) -> &[CheckedObjectType] {
        &self.checked_nominal_types
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

    /// Produces provenance handoffs for revisions compiled in this candidate.
    ///
    /// Inherited revisions are deliberately omitted: their artifacts belong to
    /// their original source snapshots and must not be rebound to this
    /// candidate. The caller provides the exact portable compatibility tuple
    /// for this compiler build; catalogue and standard-library coordinates are
    /// derived solely from the already validated candidate.
    pub fn new_function_artifact_handoffs(
        &self,
        compatibility: &ArtifactCompatibilityCoordinates,
    ) -> Result<Vec<ProvenancedFunctionArtifact>, ResolvedSourceCatalogueError> {
        let catalogue = artifact_catalogue_compatibility(&self.candidate)?;
        self.candidate
            .new_function_revisions()
            .iter()
            .map(|revision| {
                let function = self
                    .candidate
                    .candidate()
                    .function_by_id(revision.function())
                    .ok_or(ResolvedSourceCatalogueError::MissingCandidateFunction {
                        function: revision.function(),
                        revision: revision.id(),
                    })?;
                let references = self
                    .candidate
                    .references()
                    .iter()
                    .filter(|reference| {
                        reference.source_function() == revision.function()
                            && reference.source_revision() == revision.id()
                    })
                    .cloned()
                    .collect();
                let provenance = ExecutableArtifactProvenance::new(
                    self.candidate.source(),
                    catalogue,
                    compatibility.clone(),
                    revision.clone(),
                )
                .map_err(ResolvedSourceCatalogueError::ArtifactProvenance)?;
                Ok(ProvenancedFunctionArtifact {
                    definition: function.clone(),
                    revision: revision.clone(),
                    references,
                    provenance,
                })
            })
            .collect()
    }
}

fn artifact_catalogue_compatibility(
    candidate: &DeployableRevision,
) -> Result<ArtifactCatalogueCompatibility, ResolvedSourceCatalogueError> {
    let context = candidate.catalogue_hash_context();
    let standard = context.standard().map(|standard| {
        ArtifactStandardLibraryCompatibility::new(
            standard.revision(),
            standard.source().id(),
            standard.digest_version(),
            standard.digest(),
        )
    });
    ArtifactCatalogueCompatibility::new(
        candidate.candidate().revision(),
        context.version(),
        candidate.catalogue_hash(),
        standard,
    )
    .map_err(ResolvedSourceCatalogueError::ArtifactProvenance)
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
    let checked_nominal_types = checked_nominal_types(report.checked_bundle())?;
    materialize_prepared_source_catalogue(candidate, active, checked_nominal_types)
}

/// Materializes a complete compiler candidate for artifact-provenance handoff.
///
/// Unlike [`materialize_resolved_source_catalogue`], this metadata-only route
/// does not project function signatures into the current runtime invocation
/// boundary. It retains compiler-produced artifact and reference facts so they
/// can be bound to source and compatibility provenance; it neither admits nor
/// executes an artifact.
pub fn materialize_provenance_source_catalogue(
    report: &CheckReport,
    expected_base: RevisionPair,
    active: &ActiveDatabaseRevision,
) -> Result<ResolvedSourceCatalogue, ResolvedSourceCatalogueError> {
    let candidate = prepare(report, expected_base, active)?;
    let checked_nominal_types = checked_nominal_types(report.checked_bundle())?;
    materialize_prepared_provenance_catalogue(candidate, active, checked_nominal_types)
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
    let checked_nominal_types = checked_nominal_types(
        report
            .checked_bundle()
            .map(|bundle| bundle.checked_bundle()),
    )?;
    materialize_prepared_source_catalogue(candidate, active, checked_nominal_types)
}

fn materialize_prepared_source_catalogue(
    candidate: DeployableRevision,
    active: &ActiveDatabaseRevision,
    checked_nominal_types: Vec<CheckedObjectType>,
) -> Result<ResolvedSourceCatalogue, ResolvedSourceCatalogueError> {
    validate_invocation_projection(&candidate)?;
    materialize_prepared_provenance_catalogue(candidate, active, checked_nominal_types)
}

fn materialize_prepared_provenance_catalogue(
    candidate: DeployableRevision,
    active: &ActiveDatabaseRevision,
    checked_nominal_types: Vec<CheckedObjectType>,
) -> Result<ResolvedSourceCatalogue, ResolvedSourceCatalogueError> {
    let function_artifacts = materialize_function_artifacts(&candidate, active)?;
    let diff = catalogue_diff(active.catalogue(), candidate.candidate());
    Ok(ResolvedSourceCatalogue {
        base: active.catalogue().clone(),
        candidate,
        diff,
        function_artifacts,
        checked_nominal_types,
    })
}

fn checked_nominal_types(
    checked: Option<&CheckedBundle>,
) -> Result<Vec<CheckedObjectType>, ResolvedSourceCatalogueError> {
    checked
        .map(|bundle| bundle.object_types().to_vec())
        .ok_or(ResolvedSourceCatalogueError::MissingCheckedBundle)
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
    use crate::ConstantValue;
    use orna_core::{
        CatalogueRevisionId, FunctionId, FunctionRevisionId, SourceBundleId, SourceRevisionId,
        SourceUnitId,
        canonical_hash::{
            artifact_payload_digest, catalogue_digest, catalogue_digest_with_context,
            function_declaration_digest, source_bundle_digest, source_revision_record_digest,
            source_unit_content_digest,
        },
        revision::{
            ActiveDatabaseRevision, ActiveDatabaseRevisionInput, ActiveRevisionContent,
            CatalogueHashContext, CatalogueHashVersion, DefinitionIdentity,
            DefinitionReferenceKind, DefinitionReferenceTarget, DeployableRevision,
            ExecutableArtifact, ExecutableArtifactKind, FunctionRevisionRecord, RevisionPair,
            SourceOrigin, StoredSourceRevision, StoredSourceUnit, VerifiedStandardLibrarySnapshot,
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
    fn generic_public_materialization_without_functions_has_no_artifact_handoffs() {
        let active = empty_active();
        let source =
            SourceBundle::new([SourceUnit::new("tasks.orna", "CREATE SCHEMA app;")]).unwrap();
        let report = crate::check(&source, active.catalogue());
        assert!(
            report.diagnostics().is_empty(),
            "{:?}",
            report.diagnostics()
        );

        let resolved = materialize_resolved_source_catalogue(&report, active.pair(), &active)
            .expect("a no-function candidate is representable by the public materializer");

        assert!(
            resolved
                .new_function_artifact_handoffs(&test_compatibility())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn materialization_retains_checked_nominal_order_constraints_and_defaults() {
        let active = empty_active();
        let source = SourceBundle::new([SourceUnit::new(
            "types.orna",
            "CREATE SCHEMA app; CREATE TYPE app.item AS OBJECT (count INT DEFAULT 7, label TEXT NOT NULL);",
        )])
        .unwrap();
        let report = crate::check(&source, active.catalogue());
        assert!(
            report.diagnostics().is_empty(),
            "{:?}",
            report.diagnostics()
        );

        let resolved = materialize_provenance_source_catalogue(&report, active.pair(), &active)
            .expect("checked object declarations remain available to admission");
        let [item] = resolved.checked_nominal_types() else {
            panic!(
                "expected one checked nominal declaration, got {}",
                resolved.checked_nominal_types().len()
            );
        };

        assert_eq!(item.name().to_string(), "app.item");
        assert!(item.id().is_provisional());
        let fields = item.fields();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name(), "count");
        assert_eq!(fields[0].ordinal(), 0);
        assert_eq!(
            fields[0].default().unwrap().value(),
            &ConstantValue::Integer(7)
        );
        assert_eq!(fields[1].name(), "label");
        assert_eq!(fields[1].ordinal(), 1);
        assert!(!fields[1].nullable());
        assert!(fields.iter().all(|field| field.id().is_provisional()));

        // The checked view carries resolver identities only; runtime identity
        // allocation remains an admission responsibility.
        assert!(
            resolved
                .candidate()
                .object_type_by_name(
                    &orna_core::catalogue::QualifiedSemanticName::new(["app", "item"]).unwrap()
                )
                .is_some()
        );
    }

    #[test]
    fn generic_public_provenance_materialization_hands_off_scalar_function_without_runtime_admission()
     {
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

        let resolved = materialize_provenance_source_catalogue(&report, active.pair(), &active)
            .expect("compiler provenance retains scalar artifacts without runtime admission");
        let handoffs = resolved
            .new_function_artifact_handoffs(&test_compatibility())
            .unwrap();

        assert_eq!(handoffs.len(), 1);
        assert_eq!(handoffs[0].definition().name().to_string(), "app.echo");
        assert_eq!(
            handoffs[0].revision().artifact().kind(),
            ExecutableArtifactKind::Client
        );
        handoffs[0].provenance().validate().unwrap();
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

    #[test]
    fn new_function_artifact_handoff_binds_candidate_source_catalogue_and_coordinates() {
        let active = empty_active();
        let revision = FunctionRevisionId::from_bytes([0x83; 16]);
        let candidate = candidate_with_function(
            &active,
            revision,
            vec![valid_function_revision(
                revision,
                artifact_payload_digest(&[0x01, 0x02]).unwrap(),
            )],
        );
        let resolved =
            materialize_prepared_source_catalogue(candidate, &active, Vec::new()).unwrap();
        let compatibility = test_compatibility();

        let handoffs = resolved
            .new_function_artifact_handoffs(&compatibility)
            .unwrap();

        assert_eq!(handoffs.len(), 1);
        let handoff = &handoffs[0];
        assert_eq!(handoff.definition().id(), handoff.revision().function());
        assert_eq!(handoff.revision().id(), revision);
        assert!(handoff.references().is_empty());
        assert_eq!(
            handoff.provenance().source().revision(),
            resolved.source().id()
        );
        assert_eq!(
            handoff.provenance().catalogue().revision(),
            resolved.candidate().revision()
        );
        assert_eq!(
            handoff.provenance().catalogue().digest(),
            resolved.catalogue_hash()
        );
        handoff
            .provenance()
            .validate_against(
                resolved.source(),
                &artifact_catalogue_compatibility(&resolved.candidate).unwrap(),
                &compatibility,
                handoff.revision(),
            )
            .unwrap();
    }

    #[test]
    fn formatting_only_reuse_produces_no_artifact_handoff() {
        let initial_source =
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.first() RETURNS BOOLEAN RETURN TRUE;";
        let reformatted_source = "-- preserved semantics\nCREATE SCHEMA app;\nCREATE CLIENT FUNCTION app.first() RETURNS BOOL RETURN true;";
        let initial_active = empty_active();
        let initial = crate::prepare(
            &checked_report(initial_source, initial_active.catalogue()),
            initial_active.pair(),
            &initial_active,
        )
        .unwrap();
        let active = activate(&initial);
        let reused = crate::prepare(
            &checked_report(reformatted_source, active.catalogue()),
            active.pair(),
            &active,
        )
        .unwrap();

        assert!(reused.new_function_revisions().is_empty());
        assert!(
            handoff_catalogue(reused, &active)
                .new_function_artifact_handoffs(&test_compatibility())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn mixed_changed_and_reused_candidate_hands_off_only_changed_revision() {
        let initial_source = "CREATE SCHEMA app; \
            CREATE CLIENT FUNCTION app.first() RETURNS BOOLEAN RETURN TRUE; \
            CREATE CLIENT FUNCTION app.second() RETURNS BOOLEAN RETURN TRUE;";
        let changed_source = "CREATE SCHEMA app; \
            CREATE CLIENT FUNCTION app.first() RETURNS BOOL RETURN true; \
            CREATE CLIENT FUNCTION app.second() RETURNS BOOLEAN RETURN FALSE;";
        let initial_active = empty_active();
        let initial = crate::prepare(
            &checked_report(initial_source, initial_active.catalogue()),
            initial_active.pair(),
            &initial_active,
        )
        .unwrap();
        let active = activate(&initial);
        let changed = crate::prepare(
            &checked_report(changed_source, active.catalogue()),
            active.pair(),
            &active,
        )
        .unwrap();

        assert_eq!(changed.new_function_revisions().len(), 1);
        let changed_revision = changed.new_function_revisions()[0].id();
        let handoffs = handoff_catalogue(changed, &active)
            .new_function_artifact_handoffs(&test_compatibility())
            .unwrap();

        assert_eq!(handoffs.len(), 1);
        assert_eq!(handoffs[0].revision().id(), changed_revision);
        assert_eq!(handoffs[0].definition().name().to_string(), "app.second");
    }

    #[test]
    fn standard_v2_public_materialization_retains_standard_provenance() {
        let verified = crate::tests::verified_canonical_standard_source_fixture();
        let standard = crate::check_standard_library_source(&verified).unwrap();
        let active = empty_standard_application_active(&verified);
        let context =
            crate::StandardApplicationCheckContext::try_new(active.catalogue(), &standard).unwrap();
        let source = SourceBundle::new([SourceUnit::new(
            "application.orna",
            "CREATE SCHEMA app; CREATE CLIENT FUNCTION app.enabled() RETURNS BOOLEAN RETURN TRUE;",
        )])
        .unwrap();
        let report = crate::check_standard_application(&source, &context);
        assert!(
            report.diagnostics().is_empty(),
            "{:?}",
            report.diagnostics()
        );

        let resolved =
            materialize_standard_resolved_source_catalogue(&report, active.pair(), &active)
                .unwrap();
        let handoffs = resolved
            .new_function_artifact_handoffs(&test_compatibility())
            .unwrap();

        assert_eq!(handoffs.len(), 1);
        let catalogue = handoffs[0].provenance().catalogue();
        assert_eq!(catalogue.hash_version(), CatalogueHashVersion::Version2);
        let standard = catalogue
            .standard()
            .expect("version two pins standard evidence");
        assert_eq!(standard.revision(), verified.revision());
        assert_eq!(standard.source_revision(), verified.source().id());
        assert_eq!(standard.digest_version(), verified.digest_version());
        assert_eq!(standard.digest(), verified.digest());
    }

    fn test_compatibility() -> ArtifactCompatibilityCoordinates {
        ArtifactCompatibilityCoordinates::new(
            "orna.language/1",
            "orna.sys/1",
            "orna.codec/1",
            "orna.repository/1",
            "orna.storage/1",
            "orna.presentation/1",
            vec!["portable".to_owned()],
        )
        .unwrap()
    }

    fn checked_report(source: &str, base: &CatalogueSnapshot) -> CheckReport {
        let bundle = SourceBundle::new([SourceUnit::new("tasks.orna", source)]).unwrap();
        crate::check(&bundle, base)
    }

    fn activate(candidate: &DeployableRevision) -> ActiveDatabaseRevision {
        ActiveDatabaseRevision::new(
            candidate.candidate_pair(),
            candidate.source().clone(),
            candidate.candidate().clone(),
            candidate.catalogue_hash(),
            candidate.expressions().to_vec(),
            candidate.new_function_revisions().to_vec(),
            candidate.origins().to_vec(),
            candidate.references().to_vec(),
        )
        .unwrap()
    }

    fn handoff_catalogue(
        candidate: DeployableRevision,
        active: &ActiveDatabaseRevision,
    ) -> ResolvedSourceCatalogue {
        let function_artifacts = materialize_function_artifacts(&candidate, active).unwrap();
        let diff = catalogue_diff(active.catalogue(), candidate.candidate());
        ResolvedSourceCatalogue {
            base: active.catalogue().clone(),
            candidate,
            diff,
            function_artifacts,
            checked_nominal_types: Vec::new(),
        }
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
            source_revision_record_digest(
                SourceBundleId::from_bytes([0x22; 16]),
                None,
                bundle_hash,
            )
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
        let source_text = "CREATE CLIENT FUNCTION app.echo() RETURNS app.item RETURN app.item;";
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
                    origin,
                    function_declaration_digest(source_text.as_bytes()).unwrap(),
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
