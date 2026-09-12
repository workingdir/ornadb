//! Conversion-ready immutable source catalogue candidates.
//!
//! This module exposes the compiler's already validated candidate material to
//! a later runtime adapter. It deliberately carries core catalogue identities,
//! never runtime `sys.Object` identities or `ObjectRef` values.

use std::{error::Error, fmt};

use orna_core::{
    catalogue::{CatalogueSnapshot, FunctionReturn},
    catalogue_diff::{catalogue_diff, CatalogueSemanticDiff},
    revision::{
        ActiveDatabaseRevision, DefinitionOrigin, DefinitionReference, DeployableRevision,
        ExpressionArtifact, FunctionRevisionRecord, RevisionPair, Sha256Digest,
        StoredSourceRevision,
    },
    types::ResolvedType,
    FunctionId, TypeId,
};

use crate::{prepare, CheckReport, PrepareError};

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
}

impl fmt::Display for ResolvedSourceCatalogueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Preparation(error) => error.fmt(formatter),
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
        }
    }
}

impl Error for ResolvedSourceCatalogueError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Preparation(error) => Some(error),
            Self::UnsupportedReturn { .. }
            | Self::UnsupportedType { .. }
            | Self::MissingType { .. } => None,
        }
    }
}

impl From<PrepareError> for ResolvedSourceCatalogueError {
    fn from(error: PrepareError) -> Self {
        Self::Preparation(error)
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
    validate_invocation_projection(&candidate)?;
    let diff = catalogue_diff(active.catalogue(), candidate.candidate());
    Ok(ResolvedSourceCatalogue {
        base: active.catalogue().clone(),
        candidate,
        diff,
    })
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
