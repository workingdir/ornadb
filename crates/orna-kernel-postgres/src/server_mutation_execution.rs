//! Execution of the initial single-object SERVER mutation subset.
//!
//! This module accepts stable identities, typed runtime arguments, and one
//! recovered canonical mutation artifact. It does not resolve semantic names,
//! accept source SQL, or expose PostgreSQL details through its public seam.

use std::{collections::BTreeMap, error::Error, fmt};

use orna_artifact::server_mutation_plan::{
    self, MutationExpressionKind, MutationSelector, ServerDeletePlan, ServerMutationOperation,
    ServerMutationPlan,
};
use orna_core::{
    FieldId, FunctionId, FunctionRevisionId, ObjectId, ParameterId, TypeId,
    catalogue::{
        CatalogueSnapshot, FunctionDefinition, FunctionDomain, FunctionReturn, FunctionSecurity,
        FunctionTransaction, FunctionVolatility, ObjectTypeDefinition,
    },
    revision::{
        ActiveDatabaseRevision, DefinitionReferenceKind, DefinitionReferenceTarget,
        ExecutableArtifactKind, RevisionPair,
    },
    types::ResolvedType,
    value::{FunctionArgument, ResultColumn, ResultRow, ResultRows, ResultRowsError, RuntimeValue},
};
use tokio_postgres::{
    Client, IsolationLevel, Row, Statement, Transaction,
    error::SqlState,
    types::{ToSql, Type},
};

use crate::{
    PostgresKernel, PostgresKernelError,
    server_runtime::{
        ExpectedDefinitionReference, ReferenceReplayMismatch, ResolvedRuntimeType,
        configure_and_recover, postgres_type, resolve_runtime_type, runtime_types_match,
        validate_function_reference_replay,
    },
    storage::{DATA_SCHEMA, OBJECT_ID_COLUMN, field_name, relation_name, unique_constraint_name},
};

const VARIABLE_ARGUMENT_PAYLOAD_LIMIT: usize = 16 * 1024 * 1024;
const SQL_LIMIT: usize = 1024 * 1024;

#[cfg(feature = "test-hooks")]
struct MutationTestBarrier {
    reached: std::sync::Arc<tokio::sync::Barrier>,
    resume: std::sync::Arc<tokio::sync::Barrier>,
}

#[cfg(not(feature = "test-hooks"))]
struct MutationTestBarrier;

/// Immutable active state pinned for one SERVER mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServerMutationContext {
    pair: RevisionPair,
    function: FunctionId,
    function_revision: FunctionRevisionId,
}

impl ServerMutationContext {
    const fn new(
        pair: RevisionPair,
        function: FunctionId,
        function_revision: FunctionRevisionId,
    ) -> Self {
        Self {
            pair,
            function,
            function_revision,
        }
    }

    /// Returns the source and catalogue revision pair.
    pub const fn pair(&self) -> RevisionPair {
        self.pair
    }

    /// Returns the stable function identity.
    pub const fn function(&self) -> FunctionId {
        self.function
    }

    /// Returns the immutable active function revision identity.
    pub const fn function_revision(&self) -> FunctionRevisionId {
        self.function_revision
    }
}

/// Immutable active state pinned for one SERVER `INSERT` execution.
pub type ServerInsertContext = ServerMutationContext;

/// Immutable active state pinned for one SERVER `UPDATE` execution.
pub type ServerUpdateContext = ServerMutationContext;

/// Immutable active state pinned for one SERVER `DELETE` execution.
pub type ServerDeleteContext = ServerMutationContext;

/// The committed result of one validated single-row SERVER `INSERT`.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerInsertResult {
    context: ServerInsertContext,
    target: TypeId,
    object: ObjectId,
    rows: ResultRows,
}

impl ServerInsertResult {
    fn new(
        context: ServerInsertContext,
        target: TypeId,
        object: ObjectId,
        column: ResultColumn,
    ) -> Result<Self, ResultRowsError> {
        let rows = ResultRows::new(
            [column],
            [ResultRow::new([RuntimeValue::Reference { target, object }])],
        )?;
        Ok(Self {
            context,
            target,
            object,
            rows,
        })
    }

    /// Returns the complete active execution context.
    pub const fn context(&self) -> ServerInsertContext {
        self.context
    }

    /// Returns the source and catalogue revision pair.
    pub const fn pair(&self) -> RevisionPair {
        self.context.pair()
    }

    /// Returns the executed function identity.
    pub const fn function(&self) -> FunctionId {
        self.context.function()
    }

    /// Returns the immutable function revision that supplied the plan.
    pub const fn function_revision(&self) -> FunctionRevisionId {
        self.context.function_revision()
    }

    /// Returns the inserted object type identity.
    pub const fn target(&self) -> TypeId {
        self.target
    }

    /// Returns the allocated durable object identity.
    pub const fn object(&self) -> ObjectId {
        self.object
    }

    /// Returns the declared one-column, one-row typed reference result.
    pub const fn rows(&self) -> &ResultRows {
        &self.rows
    }
}

/// The committed result of one validated single-object SERVER `UPDATE`.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerUpdateResult {
    context: ServerUpdateContext,
    target: TypeId,
    selector: ObjectId,
    matched: bool,
    rows: ResultRows,
}

impl ServerUpdateResult {
    fn new(
        context: ServerUpdateContext,
        target: TypeId,
        selector: ObjectId,
        matched: bool,
        column: ResultColumn,
    ) -> Result<Self, ResultRowsError> {
        let rows = if matched {
            ResultRows::new(
                [column],
                [ResultRow::new([RuntimeValue::Reference {
                    target,
                    object: selector,
                }])],
            )?
        } else {
            ResultRows::new([column], std::iter::empty::<ResultRow>())?
        };
        Ok(Self {
            context,
            target,
            selector,
            matched,
            rows,
        })
    }

    /// Returns the complete active execution context.
    pub const fn context(&self) -> ServerUpdateContext {
        self.context
    }

    /// Returns the source and catalogue revision pair.
    pub const fn pair(&self) -> RevisionPair {
        self.context.pair()
    }

    /// Returns the executed function identity.
    pub const fn function(&self) -> FunctionId {
        self.context.function()
    }

    /// Returns the immutable function revision that supplied the plan.
    pub const fn function_revision(&self) -> FunctionRevisionId {
        self.context.function_revision()
    }

    /// Returns the updated object type identity.
    pub const fn target(&self) -> TypeId {
        self.target
    }

    /// Returns the object identity selected for update.
    pub const fn selector(&self) -> ObjectId {
        self.selector
    }

    /// Reports whether the selected object existed and was updated.
    pub const fn matched(&self) -> bool {
        self.matched
    }

    /// Returns the declared zero-or-one-row typed reference result.
    pub const fn rows(&self) -> &ResultRows {
        &self.rows
    }
}

/// The committed result of one validated single-object SERVER `DELETE`.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerDeleteResult {
    context: ServerDeleteContext,
    target: TypeId,
    selector: ObjectId,
    matched: bool,
    rows: ResultRows,
}

impl ServerDeleteResult {
    fn new(
        context: ServerDeleteContext,
        target: TypeId,
        selector: ObjectId,
        matched: bool,
        column: ResultColumn,
    ) -> Result<Self, ResultRowsError> {
        let rows = if matched {
            ResultRows::new([column], [ResultRow::new([RuntimeValue::Boolean(true)])])?
        } else {
            ResultRows::new([column], std::iter::empty::<ResultRow>())?
        };
        Ok(Self {
            context,
            target,
            selector,
            matched,
            rows,
        })
    }

    /// Returns the complete active execution context.
    pub const fn context(&self) -> ServerDeleteContext {
        self.context
    }

    /// Returns the source and catalogue revision pair.
    pub const fn pair(&self) -> RevisionPair {
        self.context.pair()
    }

    /// Returns the executed function identity.
    pub const fn function(&self) -> FunctionId {
        self.context.function()
    }

    /// Returns the immutable function revision that supplied the plan.
    pub const fn function_revision(&self) -> FunctionRevisionId {
        self.context.function_revision()
    }

    /// Returns the deleted object type identity.
    pub const fn target(&self) -> TypeId {
        self.target
    }

    /// Returns the object identity selected for deletion.
    pub const fn selector(&self) -> ObjectId {
        self.selector
    }

    /// Reports whether the selected object existed and was deleted.
    pub const fn matched(&self) -> bool {
        self.matched
    }

    /// Returns the declared zero-or-one-row typed BOOLEAN result.
    pub const fn rows(&self) -> &ResultRows {
        &self.rows
    }
}

enum ServerMutationResult {
    Insert {
        result: ServerInsertResult,
        unique_references: UniqueReferenceConstraints,
    },
    Update {
        result: ServerUpdateResult,
        unique_references: UniqueReferenceConstraints,
    },
    Delete(ServerDeleteResult),
}

impl ServerMutationResult {
    const fn context(&self) -> ServerMutationContext {
        match self {
            Self::Insert { result, .. } => result.context(),
            Self::Update { result, .. } => result.context(),
            Self::Delete(result) => result.context(),
        }
    }

    fn unique_reference_conflict(
        &self,
        source: &tokio_postgres::Error,
    ) -> Option<UniqueReferenceConstraint> {
        match self {
            Self::Insert {
                unique_references, ..
            }
            | Self::Update {
                unique_references, ..
            } => unique_references.conflict(source),
            Self::Delete(_) => None,
        }
    }

    fn committed_shutdown_error(self, source: PostgresKernelError) -> PostgresKernelError {
        match self {
            Self::Insert { result, .. } => {
                server_error(ServerMutationError::CommittedButShutdownFailed {
                    result: Box::new(result),
                    source: Box::new(source),
                })
            }
            Self::Update { result, .. } => {
                update_error(ServerUpdateError::CommittedButShutdownFailed {
                    result: Box::new(result),
                    source: Box::new(source),
                })
            }
            Self::Delete(result) => delete_error(ServerDeleteError::CommittedButShutdownFailed {
                result: Box::new(result),
                source: Box::new(source),
            }),
        }
    }

    fn into_insert(self) -> Result<ServerInsertResult, PostgresKernelError> {
        match self {
            Self::Insert { result, .. } => Ok(result),
            Self::Update { .. } | Self::Delete(_) => {
                Err(server_error(ServerMutationError::ValueInvariant {
                    rule: "INSERT execution produced a different mutation result",
                }))
            }
        }
    }

    fn into_update(self) -> Result<ServerUpdateResult, PostgresKernelError> {
        match self {
            Self::Update { result, .. } => Ok(result),
            Self::Insert { .. } | Self::Delete(_) => {
                Err(update_unavailable(PostgresKernelError::CatalogueInvariant(
                    "UPDATE execution produced a different mutation result",
                )))
            }
        }
    }

    fn into_delete(self) -> Result<ServerDeleteResult, PostgresKernelError> {
        match self {
            Self::Delete(result) => Ok(result),
            Self::Insert { .. } | Self::Update { .. } => {
                Err(delete_unavailable(PostgresKernelError::CatalogueInvariant(
                    "DELETE execution produced a different mutation result",
                )))
            }
        }
    }
}

/// The confirmed commit state attached to a SERVER mutation error.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServerMutationCommitState {
    /// PostgreSQL did not commit the mutation.
    NotCommitted,
    /// The connection failed before PostgreSQL confirmed the commit outcome.
    Unknown,
    /// PostgreSQL confirmed the commit before a later shutdown failure.
    Committed,
}

/// The confirmed commit state attached to a SERVER `INSERT` error.
pub type ServerInsertCommitState = ServerMutationCommitState;

/// The confirmed commit state attached to a SERVER `UPDATE` error.
pub type ServerUpdateCommitState = ServerMutationCommitState;

/// The confirmed commit state attached to a SERVER `DELETE` error.
pub type ServerDeleteCommitState = ServerMutationCommitState;

/// A shared typed failure while validating or executing a SERVER mutation.
#[non_exhaustive]
#[derive(Debug)]
pub enum ServerMutationError {
    /// The requested function is not in the recovered active catalogue.
    FunctionNotActive {
        /// The recovered active revision pair.
        pair: RevisionPair,
        /// The requested stable function identity.
        function: FunctionId,
    },
    /// A failure after an active function revision was pinned and rolled back.
    NotCommitted {
        /// The immutable active execution context.
        context: ServerInsertContext,
        /// The underlying typed validation or execution failure.
        source: Box<ServerInsertError>,
    },
    /// A kernel failure occurred before the change could commit.
    Kernel {
        /// The kernel failure with its native source chain.
        source: Box<PostgresKernelError>,
    },
    /// PostgreSQL failed before the commit attempt.
    Database {
        /// The PostgreSQL failure.
        source: tokio_postgres::Error,
    },
    /// The function declaration is outside the accepted mutation subset.
    FunctionSignature {
        /// The stable function identity.
        function: FunctionId,
        /// The exact rejected rule.
        rule: &'static str,
    },
    /// The active function has no exact active immutable revision record.
    CurrentRevision {
        /// The stable function identity.
        function: FunctionId,
        /// The required immutable revision identity.
        revision: FunctionRevisionId,
    },
    /// The current revision does not contain the accepted mutation artifact.
    Artifact {
        /// The stable function identity.
        function: FunctionId,
        /// The exact rejected rule.
        rule: &'static str,
    },
    /// The canonical mutation plan cannot decode.
    PlanDecode(server_mutation_plan::ServerMutationPlanError),
    /// The mutation plan disagrees with the recovered active catalogue.
    PlanInvariant {
        /// The exact rejected rule.
        rule: &'static str,
    },
    /// Durable definition references do not prove the mutation body.
    ReferenceEvidence {
        /// The stable function identity.
        function: FunctionId,
        /// The exact rejected rule.
        rule: &'static str,
    },
    /// Supplied runtime arguments do not equal the active signature.
    Argument {
        /// The related parameter identity, when one is available.
        parameter: Option<ParameterId>,
        /// The exact rejected rule.
        rule: &'static str,
    },
    /// A fixed execution or lowering limit was exceeded.
    ComplexityLimit {
        /// The bounded category.
        category: &'static str,
        /// The largest accepted value.
        maximum: usize,
    },
    /// PostgreSQL did not prepare the exact generated return shape.
    PreparedResult {
        /// The exact rejected rule.
        rule: &'static str,
    },
    /// PostgreSQL returned a value that could not be decoded.
    RowDecode {
        /// The PostgreSQL conversion failure.
        source: tokio_postgres::Error,
    },
    /// PostgreSQL returned a value that violates the generated result contract.
    ValueInvariant {
        /// The exact rejected rule.
        rule: &'static str,
    },
    /// The declared typed result could not be built.
    ResultRows(ResultRowsError),
    /// A required unique reference is already assigned to another object.
    UniqueReferenceConflict {
        /// The object type that owns the unique reference field.
        owner: TypeId,
        /// The exact unique reference field.
        field: FieldId,
        /// The object type accepted by the reference field.
        referenced_type: TypeId,
        /// The PostgreSQL integrity rejection retained as internal context.
        source: tokio_postgres::Error,
    },
    /// PostgreSQL rejected COMMIT and confirmed that the transaction did not commit.
    CommitRejected {
        /// The immutable active execution context.
        context: ServerInsertContext,
        /// The insert target identity.
        target: TypeId,
        /// The candidate object identity that did not commit.
        candidate: ObjectId,
        /// The PostgreSQL commit rejection.
        source: tokio_postgres::Error,
    },
    /// The connection failed while the commit outcome was unknown.
    CommitOutcomeUnknown {
        /// The immutable active execution context.
        context: ServerInsertContext,
        /// The insert target identity.
        target: TypeId,
        /// The candidate object identity whose commit outcome is unknown.
        candidate: ObjectId,
        /// The driver or transport failure.
        source: tokio_postgres::Error,
    },
    /// COMMIT succeeded, but the connection driver then failed to shut down.
    CommittedButShutdownFailed {
        /// The complete confirmed committed result.
        result: Box<ServerInsertResult>,
        /// The connection shutdown failure.
        source: Box<PostgresKernelError>,
    },
}

impl ServerMutationError {
    /// Returns the commit state that callers must use for retry decisions.
    pub const fn commit_state(&self) -> ServerInsertCommitState {
        match self {
            Self::CommitOutcomeUnknown { .. } => ServerInsertCommitState::Unknown,
            Self::CommittedButShutdownFailed { .. } => ServerInsertCommitState::Committed,
            Self::FunctionNotActive { .. }
            | Self::NotCommitted { .. }
            | Self::Kernel { .. }
            | Self::Database { .. }
            | Self::FunctionSignature { .. }
            | Self::CurrentRevision { .. }
            | Self::Artifact { .. }
            | Self::PlanDecode(_)
            | Self::PlanInvariant { .. }
            | Self::ReferenceEvidence { .. }
            | Self::Argument { .. }
            | Self::ComplexityLimit { .. }
            | Self::PreparedResult { .. }
            | Self::RowDecode { .. }
            | Self::ValueInvariant { .. }
            | Self::ResultRows(_)
            | Self::UniqueReferenceConflict { .. }
            | Self::CommitRejected { .. } => ServerInsertCommitState::NotCommitted,
        }
    }
}

impl fmt::Display for ServerMutationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FunctionNotActive { .. } => {
                formatter.write_str("the requested function is not active; no row was added")
            }
            Self::NotCommitted { source, .. } => {
                write!(formatter, "the row was not added: {source}")
            }
            Self::Kernel { .. } => {
                formatter.write_str("the database could not check the active function")
            }
            Self::Database { .. } => {
                formatter.write_str("the database operation failed before the change was saved")
            }
            Self::FunctionSignature { rule, .. } => {
                write!(formatter, "the function cannot perform this change: {rule}")
            }
            Self::CurrentRevision { .. } => {
                formatter.write_str("the active function definition is incomplete")
            }
            Self::Artifact { .. } => formatter.write_str(
                "the saved function is unsupported; redeploy it or contact the database administrator",
            ),
            Self::PlanDecode(_) => formatter.write_str(
                "the saved function cannot be read; redeploy it or contact the database administrator",
            ),
            Self::PlanInvariant { .. } | Self::ReferenceEvidence { .. } => formatter.write_str(
                "the saved function is inconsistent with the active database; redeploy it or contact the database administrator",
            ),
            Self::Argument { rule, .. } => {
                write!(formatter, "a supplied function argument is invalid: {rule}")
            }
            Self::ComplexityLimit { category, maximum } => {
                write!(
                    formatter,
                    "the request is too large: {category} limit is {maximum}"
                )
            }
            Self::PreparedResult { .. } => formatter.write_str(
                "the database prepared an unexpected result; redeploy the function or contact the database administrator",
            ),
            Self::RowDecode { .. } => {
                formatter.write_str("the database returned an unreadable object identity")
            }
            Self::ValueInvariant { .. } => formatter.write_str(
                "the database returned an unexpected object identity; contact the database administrator",
            ),
            Self::ResultRows(_) => formatter.write_str("the function result is invalid"),
            Self::UniqueReferenceConflict { .. } => {
                formatter.write_str("this reference is already used by another object")
            }
            Self::CommitRejected { candidate, .. } => write!(
                formatter,
                "the database rejected the final save for object {}; no row was added",
                candidate.canonical(),
            ),
            Self::CommitOutcomeUnknown { candidate, .. } => write!(
                formatter,
                "the connection failed while saving object {}; it is not known whether the row was added; do not retry automatically",
                candidate.canonical(),
            ),
            Self::CommittedButShutdownFailed { result, .. } => write!(
                formatter,
                "object {} was added, but the database connection did not close cleanly",
                result.object().canonical(),
            ),
        }
    }
}

impl Error for ServerMutationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::NotCommitted { source, .. } => Some(source),
            Self::Kernel { source } => Some(source),
            Self::Database { source }
            | Self::RowDecode { source }
            | Self::UniqueReferenceConflict { source, .. }
            | Self::CommitRejected { source, .. }
            | Self::CommitOutcomeUnknown { source, .. } => Some(source),
            Self::PlanDecode(error) => Some(error),
            Self::ResultRows(error) => Some(error),
            Self::CommittedButShutdownFailed { source, .. } => Some(source),
            Self::FunctionNotActive { .. }
            | Self::FunctionSignature { .. }
            | Self::CurrentRevision { .. }
            | Self::Artifact { .. }
            | Self::PlanInvariant { .. }
            | Self::ReferenceEvidence { .. }
            | Self::Argument { .. }
            | Self::ComplexityLimit { .. }
            | Self::PreparedResult { .. }
            | Self::ValueInvariant { .. } => None,
        }
    }
}

/// A typed failure from the initial single-row SERVER `INSERT` subset.
pub type ServerInsertError = ServerMutationError;

/// A typed failure from the initial single-object SERVER `UPDATE` subset.
#[non_exhaustive]
#[derive(Debug)]
pub enum ServerUpdateError {
    /// The database could not establish the active state needed for an update.
    Unavailable {
        /// The underlying kernel failure.
        source: Box<PostgresKernelError>,
    },
    /// The requested function is not in the recovered active catalogue.
    FunctionNotActive {
        /// The recovered active revision pair.
        pair: RevisionPair,
        /// The requested stable function identity.
        function: FunctionId,
    },
    /// A failure after an active function revision was pinned and rolled back.
    NotCommitted {
        /// The immutable active execution context.
        context: ServerUpdateContext,
        /// The shared typed mutation failure.
        source: Box<ServerMutationError>,
    },
    /// PostgreSQL rejected COMMIT and confirmed that the update did not commit.
    CommitRejected {
        /// The immutable active execution context.
        context: ServerUpdateContext,
        /// The update target identity.
        target: TypeId,
        /// The selected object identity.
        selector: ObjectId,
        /// Whether the statement matched an object before COMMIT.
        matched: bool,
        /// The PostgreSQL commit rejection.
        source: tokio_postgres::Error,
    },
    /// The connection failed while the commit outcome was unknown.
    CommitOutcomeUnknown {
        /// The immutable active execution context.
        context: ServerUpdateContext,
        /// The update target identity.
        target: TypeId,
        /// The selected object identity.
        selector: ObjectId,
        /// Whether the statement matched an object before COMMIT.
        matched: bool,
        /// The driver or transport failure.
        source: tokio_postgres::Error,
    },
    /// COMMIT succeeded, but the connection driver then failed to shut down.
    CommittedButShutdownFailed {
        /// The complete confirmed committed result.
        result: Box<ServerUpdateResult>,
        /// The connection shutdown failure.
        source: Box<PostgresKernelError>,
    },
}

impl ServerUpdateError {
    /// Returns the commit state that callers must use for retry decisions.
    pub const fn commit_state(&self) -> ServerUpdateCommitState {
        match self {
            Self::CommitOutcomeUnknown { .. } => ServerMutationCommitState::Unknown,
            Self::CommittedButShutdownFailed { .. } => ServerMutationCommitState::Committed,
            Self::Unavailable { .. }
            | Self::FunctionNotActive { .. }
            | Self::NotCommitted { .. }
            | Self::CommitRejected { .. } => ServerMutationCommitState::NotCommitted,
        }
    }
}

impl fmt::Display for ServerUpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable { .. } => {
                formatter.write_str("the database could not check the active function")
            }
            Self::FunctionNotActive { .. } => {
                formatter.write_str("the requested function is not active; no object was updated")
            }
            Self::NotCommitted { source, .. } => {
                write!(formatter, "the object was not updated: {source}")
            }
            Self::CommitRejected { selector, .. } => write!(
                formatter,
                "the database rejected the final save for object {}; the update did not commit",
                selector.canonical(),
            ),
            Self::CommitOutcomeUnknown { selector, .. } => write!(
                formatter,
                "the connection failed while saving object {}; it is not known whether the update committed; do not retry automatically",
                selector.canonical(),
            ),
            Self::CommittedButShutdownFailed { result, .. } => write!(
                formatter,
                "the update for object {} committed, but the database connection did not close cleanly",
                result.selector().canonical(),
            ),
        }
    }
}

impl Error for ServerUpdateError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Unavailable { source } => Some(source),
            Self::NotCommitted { source, .. } => Some(source),
            Self::CommitRejected { source, .. } | Self::CommitOutcomeUnknown { source, .. } => {
                Some(source)
            }
            Self::CommittedButShutdownFailed { source, .. } => Some(source),
            Self::FunctionNotActive { .. } => None,
        }
    }
}

/// A typed failure from the initial single-object SERVER `DELETE` subset.
#[non_exhaustive]
#[derive(Debug)]
pub enum ServerDeleteError {
    /// The database could not establish the active state needed for a delete.
    Unavailable {
        /// The underlying kernel failure.
        source: Box<PostgresKernelError>,
    },
    /// The requested function is not in the recovered active catalogue.
    FunctionNotActive {
        /// The recovered active revision pair.
        pair: RevisionPair,
        /// The requested stable function identity.
        function: FunctionId,
    },
    /// A failure after an active function revision was pinned and rolled back.
    NotCommitted {
        /// The immutable active execution context.
        context: ServerDeleteContext,
        /// The shared typed mutation failure.
        source: Box<ServerMutationError>,
    },
    /// A declared reference policy prevented the object from being deleted.
    DeleteRestricted {
        /// The immutable active execution context.
        context: ServerDeleteContext,
        /// The delete target identity.
        target: TypeId,
        /// The selected object identity.
        selector: ObjectId,
        /// The PostgreSQL integrity rejection retained as an internal source.
        source: tokio_postgres::Error,
    },
    /// PostgreSQL rejected COMMIT and confirmed that the delete did not commit.
    CommitRejected {
        /// The immutable active execution context.
        context: ServerDeleteContext,
        /// The delete target identity.
        target: TypeId,
        /// The selected object identity.
        selector: ObjectId,
        /// Whether the statement matched an object before COMMIT.
        matched: bool,
        /// The PostgreSQL commit rejection.
        source: tokio_postgres::Error,
    },
    /// The connection failed while the commit outcome was unknown.
    CommitOutcomeUnknown {
        /// The immutable active execution context.
        context: ServerDeleteContext,
        /// The delete target identity.
        target: TypeId,
        /// The selected object identity.
        selector: ObjectId,
        /// Whether the statement matched an object before COMMIT.
        matched: bool,
        /// The driver or transport failure.
        source: tokio_postgres::Error,
    },
    /// COMMIT succeeded, but the connection driver then failed to shut down.
    CommittedButShutdownFailed {
        /// The complete confirmed committed result.
        result: Box<ServerDeleteResult>,
        /// The connection shutdown failure.
        source: Box<PostgresKernelError>,
    },
}

impl ServerDeleteError {
    /// Returns the commit state that callers must use for retry decisions.
    pub const fn commit_state(&self) -> ServerDeleteCommitState {
        match self {
            Self::CommitOutcomeUnknown { .. } => ServerMutationCommitState::Unknown,
            Self::CommittedButShutdownFailed { .. } => ServerMutationCommitState::Committed,
            Self::Unavailable { .. }
            | Self::FunctionNotActive { .. }
            | Self::NotCommitted { .. }
            | Self::DeleteRestricted { .. }
            | Self::CommitRejected { .. } => ServerMutationCommitState::NotCommitted,
        }
    }
}

impl fmt::Display for ServerDeleteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable { .. } => {
                formatter.write_str("the database could not check the active function")
            }
            Self::FunctionNotActive { .. } => {
                formatter.write_str("the requested function is not active; no object was deleted")
            }
            Self::NotCommitted { source, .. } => {
                write!(formatter, "the object was not deleted: {source}")
            }
            Self::DeleteRestricted { selector, .. } => write!(
                formatter,
                "object {} cannot be deleted because another object still refers to it",
                selector.canonical(),
            ),
            Self::CommitRejected { selector, .. } => write!(
                formatter,
                "the database rejected the final save for object {}; the delete did not commit",
                selector.canonical(),
            ),
            Self::CommitOutcomeUnknown { selector, .. } => write!(
                formatter,
                "the connection failed while deleting object {}; it is not known whether the delete committed; do not retry automatically",
                selector.canonical(),
            ),
            Self::CommittedButShutdownFailed { result, .. } => write!(
                formatter,
                "the delete for object {} committed, but the database connection did not close cleanly",
                result.selector().canonical(),
            ),
        }
    }
}

impl Error for ServerDeleteError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Unavailable { source } => Some(source),
            Self::NotCommitted { source, .. } => Some(source),
            Self::DeleteRestricted { source, .. }
            | Self::CommitRejected { source, .. }
            | Self::CommitOutcomeUnknown { source, .. } => Some(source),
            Self::CommittedButShutdownFailed { source, .. } => Some(source),
            Self::FunctionNotActive { .. } => None,
        }
    }
}

impl PostgresKernel {
    /// Executes one active single-row SERVER `INSERT` by stable function identity.
    ///
    /// Arguments are matched by stable [`ParameterId`] and can arrive in any
    /// order. This operation does not resolve names, accept source SQL, expose
    /// an invocation identity, or provide automatic retry behaviour.
    pub async fn execute_server_insert(
        &self,
        function: FunctionId,
        arguments: &[FunctionArgument],
    ) -> Result<ServerInsertResult, PostgresKernelError> {
        self.execute_server_insert_with_options(function, arguments, None, false)
            .await
    }

    /// Pauses a live insert after it has recovered and pinned its active snapshot.
    ///
    /// This hook is compiled only for the PostgreSQL integration harness. Both
    /// barriers must have exactly two participants: the executor and the test.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn execute_server_insert_with_test_barrier(
        &self,
        function: FunctionId,
        arguments: &[FunctionArgument],
        reached: std::sync::Arc<tokio::sync::Barrier>,
        resume: std::sync::Arc<tokio::sync::Barrier>,
    ) -> Result<ServerInsertResult, PostgresKernelError> {
        self.execute_server_insert_with_options(
            function,
            arguments,
            Some(MutationTestBarrier { reached, resume }),
            false,
        )
        .await
    }

    /// Forces the driver to fail after PostgreSQL has confirmed COMMIT.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn execute_server_insert_with_forced_post_commit_driver_shutdown(
        &self,
        function: FunctionId,
        arguments: &[FunctionArgument],
    ) -> Result<ServerInsertResult, PostgresKernelError> {
        self.execute_server_insert_with_options(function, arguments, None, true)
            .await
    }

    async fn execute_server_insert_with_options(
        &self,
        function: FunctionId,
        arguments: &[FunctionArgument],
        barrier: Option<MutationTestBarrier>,
        force_post_commit_driver_shutdown: bool,
    ) -> Result<ServerInsertResult, PostgresKernelError> {
        self.execute_server_mutation_with_options(
            MutationExecutionKind::Insert,
            function,
            arguments,
            barrier,
            force_post_commit_driver_shutdown,
        )
        .await?
        .into_insert()
    }

    async fn execute_server_mutation_with_options(
        &self,
        operation: MutationExecutionKind,
        function: FunctionId,
        arguments: &[FunctionArgument],
        barrier: Option<MutationTestBarrier>,
        force_post_commit_driver_shutdown: bool,
    ) -> Result<ServerMutationResult, PostgresKernelError> {
        let mut session = self
            .open()
            .await
            .map_err(|error| pre_transaction_mutation_error(operation, error))?;
        let execution = execute_mutation_client(
            &mut session.client,
            operation,
            function,
            arguments,
            barrier.as_ref(),
        )
        .await;
        #[cfg(feature = "test-hooks")]
        if force_post_commit_driver_shutdown && execution.is_ok() {
            session.abort_driver();
        }
        #[cfg(not(feature = "test-hooks"))]
        let _ = force_post_commit_driver_shutdown;
        let shutdown = session.shutdown().await;
        match (execution, shutdown) {
            (Ok(result), Ok(())) => Ok(result),
            (Err(error), _) => Err(error),
            (Ok(result), Err(source)) => Err(result.committed_shutdown_error(source)),
        }
    }
}

impl PostgresKernel {
    /// Executes one active single-object SERVER `UPDATE` by stable function identity.
    ///
    /// Arguments are matched by stable [`ParameterId`] and can arrive in any
    /// order. A missing target commits successfully and returns zero rows.
    pub async fn execute_server_update(
        &self,
        function: FunctionId,
        arguments: &[FunctionArgument],
    ) -> Result<ServerUpdateResult, PostgresKernelError> {
        self.execute_server_mutation_with_options(
            MutationExecutionKind::Update,
            function,
            arguments,
            None,
            false,
        )
        .await?
        .into_update()
    }

    /// Executes one active single-object SERVER `DELETE` by stable function identity.
    ///
    /// Arguments are matched by stable [`ParameterId`] and can arrive in any
    /// order. A missing target commits successfully and returns zero rows.
    pub async fn execute_server_delete(
        &self,
        function: FunctionId,
        arguments: &[FunctionArgument],
    ) -> Result<ServerDeleteResult, PostgresKernelError> {
        self.execute_server_delete_with_options(function, arguments, None, false)
            .await
    }

    /// Pauses a live delete after it has recovered and pinned its active snapshot.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn execute_server_delete_with_test_barrier(
        &self,
        function: FunctionId,
        arguments: &[FunctionArgument],
        reached: std::sync::Arc<tokio::sync::Barrier>,
        resume: std::sync::Arc<tokio::sync::Barrier>,
    ) -> Result<ServerDeleteResult, PostgresKernelError> {
        self.execute_server_delete_with_options(
            function,
            arguments,
            Some(MutationTestBarrier { reached, resume }),
            false,
        )
        .await
    }

    /// Forces the driver to fail after PostgreSQL has confirmed a delete COMMIT.
    #[cfg(feature = "test-hooks")]
    #[doc(hidden)]
    pub async fn execute_server_delete_with_forced_post_commit_driver_shutdown(
        &self,
        function: FunctionId,
        arguments: &[FunctionArgument],
    ) -> Result<ServerDeleteResult, PostgresKernelError> {
        self.execute_server_delete_with_options(function, arguments, None, true)
            .await
    }

    async fn execute_server_delete_with_options(
        &self,
        function: FunctionId,
        arguments: &[FunctionArgument],
        barrier: Option<MutationTestBarrier>,
        force_post_commit_driver_shutdown: bool,
    ) -> Result<ServerDeleteResult, PostgresKernelError> {
        self.execute_server_mutation_with_options(
            MutationExecutionKind::Delete,
            function,
            arguments,
            barrier,
            force_post_commit_driver_shutdown,
        )
        .await?
        .into_delete()
    }
}

async fn execute_mutation_client(
    client: &mut Client,
    operation: MutationExecutionKind,
    function: FunctionId,
    arguments: &[FunctionArgument],
    barrier: Option<&MutationTestBarrier>,
) -> Result<ServerMutationResult, PostgresKernelError> {
    let transaction = client
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .read_only(false)
        .start()
        .await
        .map_err(|source| {
            pre_transaction_mutation_error(operation, PostgresKernelError::Database(source))
        })?;
    match execute_mutation_transaction(&transaction, operation, function, arguments, barrier).await
    {
        Ok(candidate) => commit_mutation_candidate(transaction, candidate).await,
        Err(error) => {
            let _ = transaction.rollback().await;
            Err(error)
        }
    }
}

async fn commit_mutation_candidate(
    transaction: Transaction<'_>,
    candidate: ServerMutationResult,
) -> Result<ServerMutationResult, PostgresKernelError> {
    let context = candidate.context();
    match transaction.commit().await {
        Ok(()) => Ok(candidate),
        Err(source) => {
            let unique_reference = candidate.unique_reference_conflict(&source);
            Err(match candidate {
                ServerMutationResult::Insert { .. } if let Some(reference) = unique_reference => {
                    server_error(ServerMutationError::NotCommitted {
                        context,
                        source: Box::new(reference.error(source)),
                    })
                }
                ServerMutationResult::Insert { result, .. } if source.as_db_error().is_some() => {
                    server_error(ServerMutationError::CommitRejected {
                        context,
                        target: result.target(),
                        candidate: result.object(),
                        source,
                    })
                }
                ServerMutationResult::Insert { result, .. } => {
                    server_error(ServerMutationError::CommitOutcomeUnknown {
                        context,
                        target: result.target(),
                        candidate: result.object(),
                        source,
                    })
                }
                ServerMutationResult::Update { .. } if let Some(reference) = unique_reference => {
                    update_error(ServerUpdateError::NotCommitted {
                        context,
                        source: Box::new(reference.error(source)),
                    })
                }
                ServerMutationResult::Update { result, .. } if source.as_db_error().is_some() => {
                    update_error(ServerUpdateError::CommitRejected {
                        context,
                        target: result.target(),
                        selector: result.selector(),
                        matched: result.matched(),
                        source,
                    })
                }
                ServerMutationResult::Update { result, .. } => {
                    update_error(ServerUpdateError::CommitOutcomeUnknown {
                        context,
                        target: result.target(),
                        selector: result.selector(),
                        matched: result.matched(),
                        source,
                    })
                }
                ServerMutationResult::Delete(result) => match delete_commit_failure(
                    source
                        .as_db_error()
                        .map(tokio_postgres::error::DbError::code),
                ) {
                    DeleteCommitFailure::Restricted => {
                        delete_error(ServerDeleteError::DeleteRestricted {
                            context,
                            target: result.target(),
                            selector: result.selector(),
                            source,
                        })
                    }
                    DeleteCommitFailure::Rejected => {
                        delete_error(ServerDeleteError::CommitRejected {
                            context,
                            target: result.target(),
                            selector: result.selector(),
                            matched: result.matched(),
                            source,
                        })
                    }
                    DeleteCommitFailure::Unknown => {
                        delete_error(ServerDeleteError::CommitOutcomeUnknown {
                            context,
                            target: result.target(),
                            selector: result.selector(),
                            matched: result.matched(),
                            source,
                        })
                    }
                },
            })
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeleteCommitFailure {
    Restricted,
    Rejected,
    Unknown,
}

fn delete_commit_failure(code: Option<&SqlState>) -> DeleteCommitFailure {
    match code {
        Some(code)
            if code == &SqlState::FOREIGN_KEY_VIOLATION
                || code == &SqlState::RESTRICT_VIOLATION =>
        {
            DeleteCommitFailure::Restricted
        }
        Some(_) => DeleteCommitFailure::Rejected,
        None => DeleteCommitFailure::Unknown,
    }
}

async fn execute_mutation_transaction(
    transaction: &Transaction<'_>,
    operation: MutationExecutionKind,
    function_id: FunctionId,
    arguments: &[FunctionArgument],
    barrier: Option<&MutationTestBarrier>,
) -> Result<ServerMutationResult, PostgresKernelError> {
    let active = configure_and_recover(transaction)
        .await
        .map_err(|error| mutation_kernel_error(operation, error))?;
    let function =
        active
            .catalogue()
            .function_by_id(function_id)
            .ok_or_else(|| match operation {
                MutationExecutionKind::Insert => {
                    server_error(ServerMutationError::FunctionNotActive {
                        pair: active.pair(),
                        function: function_id,
                    })
                }
                MutationExecutionKind::Update => {
                    update_error(ServerUpdateError::FunctionNotActive {
                        pair: active.pair(),
                        function: function_id,
                    })
                }
                MutationExecutionKind::Delete => {
                    delete_error(ServerDeleteError::FunctionNotActive {
                        pair: active.pair(),
                        function: function_id,
                    })
                }
            })?;
    let context =
        ServerMutationContext::new(active.pair(), function_id, function.current_revision());
    pause_after_recovery(barrier).await;
    match operation {
        MutationExecutionKind::Insert => {
            execute_active_insert(transaction, &active, function, context, arguments)
                .await
                .map(|(result, unique_references)| ServerMutationResult::Insert {
                    result,
                    unique_references,
                })
                .map_err(|error| not_committed(context, error))
        }
        MutationExecutionKind::Update => {
            execute_active_update(transaction, &active, function, context, arguments)
                .await
                .map(|(result, unique_references)| ServerMutationResult::Update {
                    result,
                    unique_references,
                })
                .map_err(|error| update_not_committed(context, error))
        }
        MutationExecutionKind::Delete => {
            execute_active_delete(transaction, &active, function, context, arguments)
                .await
                .map(ServerMutationResult::Delete)
                .map_err(|error| delete_not_committed(context, error))
        }
    }
}

async fn execute_active_update(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    context: ServerUpdateContext,
    arguments: &[FunctionArgument],
) -> Result<(ServerUpdateResult, UniqueReferenceConstraints), PostgresKernelError> {
    let validated =
        validate_active_mutation(active, function, arguments, MutationExecutionKind::Update)?;
    let selector = selector_object(&validated.plan, arguments)?;
    let lowered = lower_update_with_context(
        active.catalogue_hash_context(),
        &validated.plan,
        &validated.arguments,
    )?;
    let statement = transaction
        .prepare_typed(&lowered.sql, &lowered.bind_types)
        .await
        .map_err(|source| server_error(ServerMutationError::Database { source }))?;
    validate_prepared_result(&statement, "UPDATE")?;
    let matched = execute_update(
        transaction,
        &statement,
        lowered.binds,
        selector,
        &validated.unique_references,
    )
    .await?;
    let result = ServerUpdateResult::new(
        context,
        validated.target.id(),
        selector,
        matched,
        validated.returned.column,
    )
    .map_err(ServerMutationError::ResultRows)
    .map_err(server_error)?;
    Ok((result, validated.unique_references))
}

async fn execute_active_delete(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    context: ServerDeleteContext,
    arguments: &[FunctionArgument],
) -> Result<ServerDeleteResult, PostgresKernelError> {
    let validated = validate_active_delete(active, function, arguments)?;
    let selector = selector_argument_object(
        validated.plan.target(),
        validated.plan.selector(),
        arguments,
    )?;
    let lowered = lower_delete(&validated.plan, &validated.arguments)?;
    let statement = transaction
        .prepare_typed(&lowered.sql, &lowered.bind_types)
        .await
        .map_err(|source| server_error(ServerMutationError::Database { source }))?;
    validate_prepared_result(&statement, "DELETE")?;
    let matched = execute_delete(
        transaction,
        &statement,
        lowered.binds,
        context,
        validated.target.id(),
        selector,
    )
    .await?;
    ServerDeleteResult::new(
        context,
        validated.target.id(),
        selector,
        matched,
        validated.column,
    )
    .map_err(ServerMutationError::ResultRows)
    .map_err(server_error)
}

#[cfg(feature = "test-hooks")]
async fn pause_after_recovery(barrier: Option<&MutationTestBarrier>) {
    if let Some(barrier) = barrier {
        barrier.reached.wait().await;
        barrier.resume.wait().await;
    }
}

#[cfg(not(feature = "test-hooks"))]
async fn pause_after_recovery(_barrier: Option<&MutationTestBarrier>) {}

async fn execute_active_insert(
    transaction: &Transaction<'_>,
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    context: ServerInsertContext,
    arguments: &[FunctionArgument],
) -> Result<(ServerInsertResult, UniqueReferenceConstraints), PostgresKernelError> {
    let validated =
        validate_active_mutation(active, function, arguments, MutationExecutionKind::Insert)?;
    let lowered = lower_insert_with_context(
        active.catalogue_hash_context(),
        &validated.plan,
        &validated.arguments,
    )?;
    let statement = transaction
        .prepare_typed(&lowered.sql, &lowered.bind_types)
        .await
        .map_err(|source| server_error(ServerInsertError::Database { source }))?;
    validate_prepared_result(&statement, "INSERT")?;

    // Object allocation is deliberately after every semantic, durable,
    // argument, lowering, and prepared-result validation above.
    let object = ObjectId::new();
    let result = ServerInsertResult::new(
        context,
        validated.target.id(),
        object,
        validated.returned.column,
    )
    .map_err(ServerInsertError::ResultRows)
    .map_err(server_error)?;
    execute_insert(
        transaction,
        &statement,
        lowered.binds,
        object,
        &validated.unique_references,
    )
    .await?;
    Ok((result, validated.unique_references))
}

#[derive(Debug)]
struct ValidatedReturn {
    target: TypeId,
    column: ResultColumn,
}

struct ValidatedMutationTarget<'a> {
    target: &'a ObjectTypeDefinition,
    unique_references: UniqueReferenceConstraints,
}

struct ValidatedActiveMutation<'a> {
    returned: ValidatedReturn,
    plan: ServerMutationPlan,
    target: &'a ObjectTypeDefinition,
    unique_references: UniqueReferenceConstraints,
    arguments: BTreeMap<ParameterId, BindValue>,
}

struct ValidatedActiveDelete<'a> {
    column: ResultColumn,
    plan: ServerDeletePlan,
    target: &'a ObjectTypeDefinition,
    arguments: BTreeMap<ParameterId, BindValue>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct UniqueReferenceConstraint {
    owner: TypeId,
    field: FieldId,
    referenced_type: TypeId,
}

impl UniqueReferenceConstraint {
    fn error(self, source: tokio_postgres::Error) -> ServerMutationError {
        ServerMutationError::UniqueReferenceConflict {
            owner: self.owner,
            field: self.field,
            referenced_type: self.referenced_type,
            source,
        }
    }
}

#[derive(Clone, Debug)]
struct UniqueReferenceConstraints {
    fields: Vec<UniqueReferenceConstraint>,
}

impl UniqueReferenceConstraints {
    fn from_target(target: &ObjectTypeDefinition) -> Result<Self, PostgresKernelError> {
        let mut fields = Vec::new();
        for field in target.fields() {
            if !field.unique() {
                continue;
            }
            if !field.is_required_unique_reference() {
                return Err(plan_invariant(
                    "UNIQUE target fields must be required typed references",
                ));
            }
            let Some(referenced_type) = field.resolved_type().reference_target() else {
                return Err(plan_invariant(
                    "UNIQUE target fields must be required typed references",
                ));
            };
            fields.push(UniqueReferenceConstraint {
                owner: target.id(),
                field: field.id(),
                referenced_type,
            });
        }
        Ok(Self { fields })
    }

    fn conflict(&self, source: &tokio_postgres::Error) -> Option<UniqueReferenceConstraint> {
        let error = source.as_db_error()?;
        unique_reference_constraint(self, Some(error.code()), error.constraint())
    }
}

fn unique_reference_constraint(
    constraints: &UniqueReferenceConstraints,
    code: Option<&SqlState>,
    constraint: Option<&str>,
) -> Option<UniqueReferenceConstraint> {
    if code != Some(&SqlState::UNIQUE_VIOLATION) {
        return None;
    }
    let constraint = constraint?;
    constraints
        .fields
        .iter()
        .copied()
        .find(|expected| unique_constraint_name(expected.field) == constraint)
}

fn mutation_database_error(
    source: tokio_postgres::Error,
    constraints: &UniqueReferenceConstraints,
) -> ServerMutationError {
    if let Some(reference) = constraints.conflict(&source) {
        reference.error(source)
    } else {
        ServerMutationError::Database { source }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MutationExecutionKind {
    Insert,
    Update,
    Delete,
}

impl MutationExecutionKind {
    const fn artifact_version(self) -> u32 {
        match self {
            Self::Insert => server_mutation_plan::INSERT_FORMAT_VERSION,
            Self::Update => server_mutation_plan::UPDATE_FORMAT_VERSION,
            Self::Delete => server_mutation_plan::DELETE_FORMAT_VERSION,
        }
    }
}

fn validate_active_mutation<'a>(
    active: &'a ActiveDatabaseRevision,
    function: &FunctionDefinition,
    arguments: &[FunctionArgument],
    operation: MutationExecutionKind,
) -> Result<ValidatedActiveMutation<'a>, PostgresKernelError> {
    let context = active.catalogue_hash_context();
    let returned =
        validate_function_signature_for_context(context, active.catalogue(), function, operation)?;
    let revision = active
        .function_revisions()
        .iter()
        .find(|revision| {
            revision.function() == function.id() && revision.id() == function.current_revision()
        })
        .ok_or_else(|| {
            server_error(ServerMutationError::CurrentRevision {
                function: function.id(),
                revision: function.current_revision(),
            })
        })?;
    let artifact = revision.artifact();
    validate_artifact_metadata_for_operation(
        function.id(),
        artifact.kind(),
        artifact.format(),
        artifact.version(),
        revision.language_version(),
        operation,
    )?;
    let plan = ServerMutationPlan::decode(artifact.payload())
        .map_err(ServerMutationError::PlanDecode)
        .map_err(server_error)?;
    let target = validate_plan_for_context(
        context,
        active.catalogue(),
        function,
        returned.target,
        &plan,
        operation,
    )?;
    validate_reference_evidence(active, function, &plan)?;
    let arguments =
        validate_arguments_with_context(context, active.catalogue(), function, arguments)?;
    Ok(ValidatedActiveMutation {
        returned,
        plan,
        target: target.target,
        unique_references: target.unique_references,
        arguments,
    })
}

fn validate_active_delete<'a>(
    active: &'a ActiveDatabaseRevision,
    function: &FunctionDefinition,
    arguments: &[FunctionArgument],
) -> Result<ValidatedActiveDelete<'a>, PostgresKernelError> {
    let context = active.catalogue_hash_context();
    let column =
        validate_delete_function_signature_with_context(context, active.catalogue(), function)?;
    let revision = active
        .function_revisions()
        .iter()
        .find(|revision| {
            revision.function() == function.id() && revision.id() == function.current_revision()
        })
        .ok_or_else(|| {
            server_error(ServerMutationError::CurrentRevision {
                function: function.id(),
                revision: function.current_revision(),
            })
        })?;
    let artifact = revision.artifact();
    validate_artifact_metadata_for_operation(
        function.id(),
        artifact.kind(),
        artifact.format(),
        artifact.version(),
        revision.language_version(),
        MutationExecutionKind::Delete,
    )?;
    let plan = ServerDeletePlan::decode(artifact.payload())
        .map_err(ServerMutationError::PlanDecode)
        .map_err(server_error)?;
    let target = validate_delete_plan(active.catalogue(), function, &plan)?;
    validate_delete_reference_evidence(active, function, &plan)?;
    let arguments =
        validate_arguments_with_context(context, active.catalogue(), function, arguments)?;
    Ok(ValidatedActiveDelete {
        column,
        plan,
        target,
        arguments,
    })
}

#[cfg(test)]
fn validate_artifact_metadata(
    function: FunctionId,
    kind: ExecutableArtifactKind,
    format: &str,
    version: u32,
    language_version: &str,
) -> Result<(), PostgresKernelError> {
    validate_artifact_metadata_for_operation(
        function,
        kind,
        format,
        version,
        language_version,
        MutationExecutionKind::Insert,
    )
}

fn validate_artifact_metadata_for_operation(
    function: FunctionId,
    kind: ExecutableArtifactKind,
    format: &str,
    version: u32,
    language_version: &str,
    operation: MutationExecutionKind,
) -> Result<(), PostgresKernelError> {
    if kind != ExecutableArtifactKind::Server {
        return Err(artifact_error(
            function,
            "the active function must contain SERVER executable data",
        ));
    }
    if format != server_mutation_plan::FORMAT_IDENTITY || version != operation.artifact_version() {
        return Err(artifact_error(
            function,
            match operation {
                MutationExecutionKind::Insert => {
                    "the active function must use the supported INSERT mutation format version 1"
                }
                MutationExecutionKind::Update => {
                    "the active function must use the supported UPDATE mutation format version 2"
                }
                MutationExecutionKind::Delete => {
                    "the active function must use the supported DELETE mutation format version 3"
                }
            },
        ));
    }
    if language_version != server_mutation_plan::LANGUAGE_VERSION_IDENTITY {
        return Err(artifact_error(
            function,
            "the active function must use orna.language/1",
        ));
    }
    Ok(())
}

#[cfg(test)]
fn validate_function_signature(
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
) -> Result<ValidatedReturn, PostgresKernelError> {
    validate_function_signature_for_operation(catalogue, function, MutationExecutionKind::Insert)
}

#[cfg(test)]
fn validate_function_signature_for_operation(
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
    operation: MutationExecutionKind,
) -> Result<ValidatedReturn, PostgresKernelError> {
    validate_function_signature_for_context(
        &orna_core::revision::CatalogueHashContext::version_one(),
        catalogue,
        function,
        operation,
    )
}

fn validate_function_signature_for_context(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
    operation: MutationExecutionKind,
) -> Result<ValidatedReturn, PostgresKernelError> {
    validate_mutation_function_header(context, catalogue, function, operation)?;
    let reject = |rule| function_signature_error(function.id(), rule);
    let FunctionReturn::Rows(columns) = function.return_type() else {
        return Err(reject(match operation {
            MutationExecutionKind::Insert => {
                "an INSERT SERVER function must return exactly one object-reference column"
            }
            MutationExecutionKind::Update => {
                "an UPDATE SERVER function must return exactly one object-reference column"
            }
            MutationExecutionKind::Delete => {
                "a DELETE SERVER function must return exactly one BOOLEAN column"
            }
        }));
    };
    let [column] = columns.as_slice() else {
        return Err(reject(match operation {
            MutationExecutionKind::Insert => {
                "an INSERT SERVER function must return exactly one object-reference column"
            }
            MutationExecutionKind::Update => {
                "an UPDATE SERVER function must return exactly one object-reference column"
            }
            MutationExecutionKind::Delete => {
                "a DELETE SERVER function must return exactly one BOOLEAN column"
            }
        }));
    };
    let ResolvedRuntimeType::Reference(target) =
        resolve_runtime_type(context, column.resolved_type())
    else {
        return Err(reject(
            "the sole result column must be a non-null object reference",
        ));
    };
    if catalogue.object_type_by_id(target).is_none() {
        return Err(reject(
            "the result column must reference an active object type",
        ));
    }
    let column = ResultColumn::new(column.name(), ResolvedType::reference(target), false)
        .map_err(ServerInsertError::ResultRows)
        .map_err(server_error)?;
    Ok(ValidatedReturn { target, column })
}

#[cfg(test)]
fn validate_delete_function_signature(
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
) -> Result<ResultColumn, PostgresKernelError> {
    validate_delete_function_signature_with_context(
        &orna_core::revision::CatalogueHashContext::version_one(),
        catalogue,
        function,
    )
}

fn validate_delete_function_signature_with_context(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
) -> Result<ResultColumn, PostgresKernelError> {
    validate_mutation_function_header(context, catalogue, function, MutationExecutionKind::Delete)?;
    let FunctionReturn::Rows(columns) = function.return_type() else {
        return Err(function_signature_error(
            function.id(),
            "a DELETE SERVER function must return exactly one BOOLEAN column",
        ));
    };
    let [column] = columns.as_slice() else {
        return Err(function_signature_error(
            function.id(),
            "a DELETE SERVER function must return exactly one BOOLEAN column",
        ));
    };
    if !runtime_types_match(
        context,
        column.resolved_type(),
        ResolvedType::scalar(orna_core::types::StandardScalar::Boolean),
    ) {
        return Err(function_signature_error(
            function.id(),
            "the sole DELETE result column must be BOOLEAN",
        ));
    }
    ResultColumn::new(
        column.name(),
        ResolvedType::Scalar(orna_core::types::StandardScalar::Boolean),
        false,
    )
    .map_err(ServerMutationError::ResultRows)
    .map_err(server_error)
}

fn validate_mutation_function_header(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
    operation: MutationExecutionKind,
) -> Result<(), PostgresKernelError> {
    let reject = |rule| function_signature_error(function.id(), rule);
    if function.domain() != FunctionDomain::Server {
        return Err(reject("this operation requires a SERVER function"));
    }
    if function.security() != FunctionSecurity::Invoker {
        return Err(reject(match operation {
            MutationExecutionKind::Insert => "an INSERT SERVER function must use SECURITY INVOKER",
            MutationExecutionKind::Update => "an UPDATE SERVER function must use SECURITY INVOKER",
            MutationExecutionKind::Delete => "a DELETE SERVER function must use SECURITY INVOKER",
        }));
    }
    if function.transaction() != Some(FunctionTransaction::Atomic) {
        return Err(reject(match operation {
            MutationExecutionKind::Insert => {
                "an INSERT SERVER function must use exactly TRANSACTION ATOMIC"
            }
            MutationExecutionKind::Update => {
                "an UPDATE SERVER function must use exactly TRANSACTION ATOMIC"
            }
            MutationExecutionKind::Delete => {
                "a DELETE SERVER function must use exactly TRANSACTION ATOMIC"
            }
        }));
    }
    if function.volatility() != FunctionVolatility::Volatile {
        return Err(reject(match operation {
            MutationExecutionKind::Insert => {
                "an INSERT SERVER function must use VOLATILITY VOLATILE"
            }
            MutationExecutionKind::Update => {
                "an UPDATE SERVER function must use VOLATILITY VOLATILE"
            }
            MutationExecutionKind::Delete => {
                "a DELETE SERVER function must use VOLATILITY VOLATILE"
            }
        }));
    }
    for parameter in function.parameters() {
        if parameter.default_expression().is_some() {
            return Err(reject(match operation {
                MutationExecutionKind::Insert => {
                    "INSERT SERVER function parameters cannot have default expressions"
                }
                MutationExecutionKind::Update => {
                    "UPDATE SERVER function parameters cannot have default expressions"
                }
                MutationExecutionKind::Delete => {
                    "DELETE SERVER function parameters cannot have default expressions"
                }
            }));
        }
        if !runtime_type_is_active(context, catalogue, parameter.resolved_type()) {
            return Err(reject(match operation {
                MutationExecutionKind::Insert => {
                    "every INSERT SERVER function parameter must use a supported active type"
                }
                MutationExecutionKind::Update => {
                    "every UPDATE SERVER function parameter must use a supported active type"
                }
                MutationExecutionKind::Delete => {
                    "every DELETE SERVER function parameter must use a supported active type"
                }
            }));
        }
    }
    Ok(())
}

fn function_signature_error(function: FunctionId, rule: &'static str) -> PostgresKernelError {
    server_error(ServerMutationError::FunctionSignature { function, rule })
}

fn runtime_type_is_active(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    resolved_type: ResolvedType,
) -> bool {
    if postgres_type(resolve_runtime_type(context, resolved_type)).is_none() {
        return false;
    }
    match resolve_runtime_type(context, resolved_type) {
        ResolvedRuntimeType::Reference(target) => catalogue.object_type_by_id(target).is_some(),
        ResolvedRuntimeType::LegacyScalar(_) | ResolvedRuntimeType::VerifiedValue { .. } => true,
        ResolvedRuntimeType::Unsupported => false,
    }
}

fn validate_active_runtime_type(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    resolved_type: ResolvedType,
    rule: &'static str,
) -> Result<(), PostgresKernelError> {
    if postgres_type(resolve_runtime_type(context, resolved_type)).is_none() {
        return Err(plan_invariant(rule));
    }
    match resolve_runtime_type(context, resolved_type) {
        ResolvedRuntimeType::Reference(target) if catalogue.object_type_by_id(target).is_none() => {
            return Err(plan_invariant(
                "every referenced object type must be active",
            ));
        }
        ResolvedRuntimeType::LegacyScalar(_)
        | ResolvedRuntimeType::VerifiedValue { .. }
        | ResolvedRuntimeType::Reference(_) => {}
        ResolvedRuntimeType::Unsupported => return Err(plan_invariant(rule)),
    }
    Ok(())
}

#[cfg(test)]
fn validate_plan<'a>(
    catalogue: &'a CatalogueSnapshot,
    function: &FunctionDefinition,
    returned_target: TypeId,
    plan: &ServerMutationPlan,
) -> Result<&'a ObjectTypeDefinition, PostgresKernelError> {
    Ok(validate_plan_for_operation(
        catalogue,
        function,
        returned_target,
        plan,
        MutationExecutionKind::Insert,
    )?
    .target)
}

fn validate_plan_for_context<'a>(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &'a CatalogueSnapshot,
    function: &FunctionDefinition,
    returned_target: TypeId,
    plan: &ServerMutationPlan,
    operation: MutationExecutionKind,
) -> Result<ValidatedMutationTarget<'a>, PostgresKernelError> {
    let operation_matches = matches!(
        (operation, plan.operation()),
        (
            MutationExecutionKind::Insert,
            ServerMutationOperation::Insert
        ) | (
            MutationExecutionKind::Update,
            ServerMutationOperation::Update { .. }
        )
    );
    if !operation_matches || plan.format_version() != operation.artifact_version() {
        return Err(plan_invariant(
            "the payload operation and version must match the requested mutation",
        ));
    }
    if plan.returned_object() != plan.target() || plan.target() != returned_target {
        return Err(plan_invariant(
            "plan target, returned object, and declared result REF target must match",
        ));
    }
    let target = catalogue
        .object_type_by_id(plan.target())
        .ok_or_else(|| plan_invariant("mutation target must be an active object type"))?;
    let unique_references = UniqueReferenceConstraints::from_target(target)?;
    for field in target.fields() {
        if field.default_expression().is_some() {
            return Err(plan_invariant(
                "mutation targets cannot contain field default expressions",
            ));
        }
        match resolve_runtime_type(context, field.resolved_type()) {
            ResolvedRuntimeType::Reference(target)
                if catalogue.object_type_by_id(target).is_none() =>
            {
                return Err(plan_invariant(
                    "every target-field REF type must name an active object type",
                ));
            }
            ResolvedRuntimeType::LegacyScalar(_)
            | ResolvedRuntimeType::VerifiedValue { .. }
            | ResolvedRuntimeType::Reference(_)
            | ResolvedRuntimeType::Unsupported => {}
        }
    }

    let mut assigned = BTreeMap::new();
    for assignment in plan.assignments() {
        if assignment.owner() != target.id() {
            return Err(plan_invariant(
                "every assignment owner must equal the mutation target",
            ));
        }
        let field = target
            .field_by_id(assignment.field())
            .ok_or_else(|| plan_invariant("every owner-qualified assigned field must be active"))?;
        if assigned.insert(field.id(), ()).is_some() {
            return Err(plan_invariant(
                "an owner-qualified field cannot be assigned more than once",
            ));
        }
        let expression = assignment.expression();
        validate_active_runtime_type(
            context,
            catalogue,
            expression.resolved_type(),
            "every assignment expression must use the active runtime subset",
        )?;
        if !runtime_types_match(context, expression.resolved_type(), field.resolved_type()) {
            return Err(plan_invariant(
                "assignment expression type must exactly equal its target field type",
            ));
        }
        if matches!(expression.kind(), MutationExpressionKind::TypedNull) && !field.nullable() {
            return Err(plan_invariant(
                "typed NULL can target only a nullable field",
            ));
        }
        if let MutationExpressionKind::Parameter { owner, parameter } = expression.kind() {
            if *owner != function.id() {
                return Err(plan_invariant(
                    "parameter expression owner must equal the active function",
                ));
            }
            let parameter = function.parameter_by_id(*parameter).ok_or_else(|| {
                plan_invariant("parameter expression must name an active declared parameter")
            })?;
            if parameter.default_expression().is_some()
                || !runtime_types_match(
                    context,
                    parameter.resolved_type(),
                    expression.resolved_type(),
                )
            {
                return Err(plan_invariant(
                    "parameter expression must exactly match a required active parameter",
                ));
            }
        }
    }
    if operation == MutationExecutionKind::Insert
        && target
            .fields()
            .iter()
            .any(|field| !field.nullable() && !assigned.contains_key(&field.id()))
    {
        return Err(plan_invariant(
            "every non-null target field must have an assignment",
        ));
    }
    if let ServerMutationOperation::Update { selector } = plan.operation() {
        if selector.owner() != function.id() {
            return Err(plan_invariant(
                "selector owner must equal the active function",
            ));
        }
        let parameter = function
            .parameter_by_id(selector.parameter())
            .ok_or_else(|| plan_invariant("selector must name an active declared parameter"))?;
        if parameter.default_expression().is_some()
            || parameter.resolved_type() != ResolvedType::reference(target.id())
        {
            return Err(plan_invariant(
                "selector must exactly match a required REF parameter for the target object",
            ));
        }
    }
    Ok(ValidatedMutationTarget {
        target,
        unique_references,
    })
}

#[cfg(test)]
fn validate_plan_for_operation<'a>(
    catalogue: &'a CatalogueSnapshot,
    function: &FunctionDefinition,
    returned_target: TypeId,
    plan: &ServerMutationPlan,
    operation: MutationExecutionKind,
) -> Result<ValidatedMutationTarget<'a>, PostgresKernelError> {
    validate_plan_for_context(
        &orna_core::revision::CatalogueHashContext::version_one(),
        catalogue,
        function,
        returned_target,
        plan,
        operation,
    )
}

fn validate_delete_plan<'a>(
    catalogue: &'a CatalogueSnapshot,
    function: &FunctionDefinition,
    plan: &ServerDeletePlan,
) -> Result<&'a ObjectTypeDefinition, PostgresKernelError> {
    if plan.format_version() != server_mutation_plan::DELETE_FORMAT_VERSION {
        return Err(plan_invariant(
            "the DELETE payload must use mutation format version 3",
        ));
    }
    let target = catalogue
        .object_type_by_id(plan.target())
        .ok_or_else(|| plan_invariant("DELETE target must be an active object type"))?;
    let selector = plan.selector();
    if selector.owner() != function.id() {
        return Err(plan_invariant(
            "DELETE selector owner must equal the active function",
        ));
    }
    let parameter = function
        .parameter_by_id(selector.parameter())
        .ok_or_else(|| plan_invariant("DELETE selector must name an active declared parameter"))?;
    if parameter.default_expression().is_some()
        || parameter.resolved_type() != ResolvedType::reference(target.id())
    {
        return Err(plan_invariant(
            "DELETE selector must exactly match a required REF parameter for the target object",
        ));
    }
    Ok(target)
}

fn validate_reference_evidence(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    plan: &ServerMutationPlan,
) -> Result<(), PostgresKernelError> {
    let expected = expected_body_references(plan);
    validate_function_reference_replay(active, function, &expected).map_err(|mismatch| {
        let rule = match mismatch {
            ReferenceReplayMismatch::Count => {
                "reference count must match the signature and mutation body"
            }
            ReferenceReplayMismatch::Sequence => {
                "references must replay the exact signature and mutation body order"
            }
        };
        server_error(ServerInsertError::ReferenceEvidence {
            function: function.id(),
            rule,
        })
    })
}

fn validate_delete_reference_evidence(
    active: &ActiveDatabaseRevision,
    function: &FunctionDefinition,
    plan: &ServerDeletePlan,
) -> Result<(), PostgresKernelError> {
    let expected = expected_delete_body_references(plan);
    validate_function_reference_replay(active, function, &expected).map_err(|mismatch| {
        let rule = match mismatch {
            ReferenceReplayMismatch::Count => {
                "reference count must match the signature and DELETE body"
            }
            ReferenceReplayMismatch::Sequence => {
                "references must replay the exact signature and DELETE body order"
            }
        };
        server_error(ServerMutationError::ReferenceEvidence {
            function: function.id(),
            rule,
        })
    })
}

fn expected_delete_body_references(plan: &ServerDeletePlan) -> [ExpectedDefinitionReference; 3] {
    [
        ExpectedDefinitionReference::new(
            DefinitionReferenceKind::WriteObject,
            DefinitionReferenceTarget::ObjectType(plan.target()),
        ),
        ExpectedDefinitionReference::new(
            DefinitionReferenceKind::ObjectReference,
            DefinitionReferenceTarget::ObjectType(plan.target()),
        ),
        ExpectedDefinitionReference::new(
            DefinitionReferenceKind::ParameterRead,
            DefinitionReferenceTarget::Parameter {
                owner: plan.selector().owner(),
                parameter: plan.selector().parameter(),
            },
        ),
    ]
}

fn expected_body_references(plan: &ServerMutationPlan) -> Vec<ExpectedDefinitionReference> {
    let mut expected = Vec::with_capacity(plan.assignments().len().saturating_mul(2) + 4);
    expected.push(ExpectedDefinitionReference::new(
        DefinitionReferenceKind::WriteObject,
        DefinitionReferenceTarget::ObjectType(plan.target()),
    ));
    for assignment in plan.assignments() {
        expected.push(ExpectedDefinitionReference::new(
            DefinitionReferenceKind::WriteField,
            DefinitionReferenceTarget::Field {
                owner: assignment.owner(),
                field: assignment.field(),
            },
        ));
        if let MutationExpressionKind::Parameter { owner, parameter } =
            assignment.expression().kind()
        {
            expected.push(ExpectedDefinitionReference::new(
                DefinitionReferenceKind::ParameterRead,
                DefinitionReferenceTarget::Parameter {
                    owner: *owner,
                    parameter: *parameter,
                },
            ));
        }
    }
    if let ServerMutationOperation::Update { selector } = plan.operation() {
        expected.push(ExpectedDefinitionReference::new(
            DefinitionReferenceKind::ObjectReference,
            DefinitionReferenceTarget::ObjectType(plan.target()),
        ));
        expected.push(ExpectedDefinitionReference::new(
            DefinitionReferenceKind::ParameterRead,
            DefinitionReferenceTarget::Parameter {
                owner: selector.owner(),
                parameter: selector.parameter(),
            },
        ));
    }
    expected.push(ExpectedDefinitionReference::new(
        DefinitionReferenceKind::ObjectReference,
        DefinitionReferenceTarget::ObjectType(plan.returned_object()),
    ));
    expected
}

fn validate_arguments_with_context(
    context: &orna_core::revision::CatalogueHashContext,
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
    arguments: &[FunctionArgument],
) -> Result<BTreeMap<ParameterId, BindValue>, PostgresKernelError> {
    let mut validated = BTreeMap::new();
    let mut variable_payload = 0usize;
    for argument in arguments {
        let parameter_id = argument.parameter();
        if validated.contains_key(&parameter_id) {
            return Err(argument_error(
                Some(parameter_id),
                "the same parameter was supplied twice",
            ));
        }
        let parameter = function.parameter_by_id(parameter_id).ok_or_else(|| {
            argument_error(
                Some(parameter_id),
                "an argument was supplied for a parameter that this function does not declare",
            )
        })?;
        let value = argument.value();
        if value.is_null() {
            return Err(argument_error(
                Some(parameter_id),
                "function arguments cannot be NULL",
            ));
        }
        if !runtime_type_is_active(context, catalogue, value.resolved_type()) {
            return Err(argument_error(
                Some(parameter_id),
                "the argument type is unsupported or its referenced object type is inactive",
            ));
        }
        if !runtime_types_match(context, value.resolved_type(), parameter.resolved_type()) {
            return Err(argument_error(
                Some(parameter_id),
                "the argument type does not match the declared parameter type",
            ));
        }
        variable_payload = variable_payload
            .checked_add(variable_payload_len(value)?)
            .ok_or_else(payload_limit_error)?;
        if variable_payload > VARIABLE_ARGUMENT_PAYLOAD_LIMIT {
            return Err(payload_limit_error());
        }
        validated.insert(parameter_id, BindValue::from_runtime(value, parameter_id)?);
    }
    for parameter in function.parameters() {
        if !validated.contains_key(&parameter.id()) {
            return Err(argument_error(
                Some(parameter.id()),
                "a required argument is missing",
            ));
        }
    }
    Ok(validated)
}

#[cfg(test)]
fn validate_arguments(
    catalogue: &CatalogueSnapshot,
    function: &FunctionDefinition,
    arguments: &[FunctionArgument],
) -> Result<BTreeMap<ParameterId, BindValue>, PostgresKernelError> {
    validate_arguments_with_context(
        &orna_core::revision::CatalogueHashContext::version_one(),
        catalogue,
        function,
        arguments,
    )
}

fn selector_object(
    plan: &ServerMutationPlan,
    arguments: &[FunctionArgument],
) -> Result<ObjectId, PostgresKernelError> {
    let selector = plan
        .selector()
        .ok_or_else(|| plan_invariant("UPDATE plan must contain one selector parameter"))?;
    selector_argument_object(plan.target(), selector, arguments)
}

fn selector_argument_object(
    target: TypeId,
    selector: MutationSelector,
    arguments: &[FunctionArgument],
) -> Result<ObjectId, PostgresKernelError> {
    let argument = arguments
        .iter()
        .find(|argument| argument.parameter() == selector.parameter())
        .ok_or_else(|| plan_invariant("validated selector argument must be present"))?;
    match argument.value() {
        RuntimeValue::Reference {
            target: actual,
            object,
        } if *actual == target => Ok(*object),
        _ => Err(plan_invariant(
            "validated selector argument must be an exact target object reference",
        )),
    }
}

fn variable_payload_len(value: &RuntimeValue) -> Result<usize, PostgresKernelError> {
    match value {
        RuntimeValue::Text(value) => Ok(value.len()),
        RuntimeValue::Bytes(value) => Ok(value.len()),
        RuntimeValue::Null(_)
        | RuntimeValue::Boolean(_)
        | RuntimeValue::Integer(_)
        | RuntimeValue::BigInt(_)
        | RuntimeValue::Float(_)
        | RuntimeValue::Reference { .. } => Ok(0),
        _ => Err(argument_error(None, "the argument type is unsupported")),
    }
}

fn payload_limit_error() -> PostgresKernelError {
    server_error(ServerInsertError::ComplexityLimit {
        category: "total size of text and binary arguments",
        maximum: VARIABLE_ARGUMENT_PAYLOAD_LIMIT,
    })
}

#[derive(Clone, Debug, PartialEq)]
enum BindValue {
    Boolean(bool),
    Integer(i32),
    BigInt(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
}

impl BindValue {
    fn from_runtime(
        value: &RuntimeValue,
        parameter: ParameterId,
    ) -> Result<Self, PostgresKernelError> {
        match value {
            RuntimeValue::Boolean(value) => Ok(Self::Boolean(*value)),
            RuntimeValue::Integer(value) => Ok(Self::Integer(*value)),
            RuntimeValue::BigInt(value) => Ok(Self::BigInt(*value)),
            RuntimeValue::Float(value) => Ok(Self::Float(value.value())),
            RuntimeValue::Text(value) => Ok(Self::Text(value.clone())),
            RuntimeValue::Bytes(value) => Ok(Self::Bytes(value.clone())),
            RuntimeValue::Reference { object, .. } => Ok(Self::Bytes(object.to_bytes().to_vec())),
            RuntimeValue::Null(_) => Err(argument_error(
                Some(parameter),
                "function arguments cannot be NULL",
            )),
            _ => Err(argument_error(
                Some(parameter),
                "the argument type is unsupported",
            )),
        }
    }

    fn as_to_sql(&self) -> &(dyn ToSql + Sync) {
        match self {
            Self::Boolean(value) => value,
            Self::Integer(value) => value,
            Self::BigInt(value) => value,
            Self::Float(value) => value,
            Self::Text(value) => value,
            Self::Bytes(value) => value,
        }
    }
}

struct LoweredMutation {
    sql: String,
    bind_types: Vec<Type>,
    binds: Vec<BindValue>,
}

fn lower_insert_with_context(
    context: &orna_core::revision::CatalogueHashContext,
    plan: &ServerMutationPlan,
    arguments: &BTreeMap<ParameterId, BindValue>,
) -> Result<LoweredMutation, PostgresKernelError> {
    let mut columns = vec![String::from(OBJECT_ID_COLUMN)];
    let mut values = vec![String::from("$1")];
    let mut bind_types = vec![Type::BYTEA];
    let mut binds = Vec::new();
    let mut parameter_placeholders = BTreeMap::new();
    for assignment in plan.assignments() {
        columns.push(field_name(assignment.field()));
        values.push(lower_assignment_expression(
            context,
            assignment.expression(),
            arguments,
            &mut bind_types,
            &mut binds,
            &mut parameter_placeholders,
        )?);
    }
    let sql = format!(
        "INSERT INTO {DATA_SCHEMA}.{} ({}) VALUES ({}) RETURNING {OBJECT_ID_COLUMN} AS c0",
        relation_name(plan.target()),
        columns.join(", "),
        values.join(", "),
    );
    if sql.len() > SQL_LIMIT {
        return Err(server_error(ServerInsertError::ComplexityLimit {
            category: "saved function complexity",
            maximum: SQL_LIMIT,
        }));
    }
    Ok(LoweredMutation {
        sql,
        bind_types,
        binds,
    })
}

#[cfg(test)]
fn lower_insert(
    plan: &ServerMutationPlan,
    arguments: &BTreeMap<ParameterId, BindValue>,
) -> Result<LoweredMutation, PostgresKernelError> {
    lower_insert_with_context(
        &orna_core::revision::CatalogueHashContext::version_one(),
        plan,
        arguments,
    )
}

fn lower_assignment_expression(
    context: &orna_core::revision::CatalogueHashContext,
    expression: &server_mutation_plan::MutationExpression,
    arguments: &BTreeMap<ParameterId, BindValue>,
    bind_types: &mut Vec<Type>,
    binds: &mut Vec<BindValue>,
    parameter_placeholders: &mut BTreeMap<ParameterId, usize>,
) -> Result<String, PostgresKernelError> {
    let value_type = postgres_type(resolve_runtime_type(context, expression.resolved_type()))
        .ok_or_else(|| {
            plan_invariant("the assignment type cannot be stored by the initial runtime")
        })?;
    match expression.kind() {
        MutationExpressionKind::Parameter { parameter, .. } => parameter_placeholder(
            *parameter,
            value_type,
            arguments,
            bind_types,
            binds,
            parameter_placeholders,
        ),
        MutationExpressionKind::BooleanLiteral { value } => {
            binds.push(BindValue::Boolean(*value));
            bind_types.push(value_type);
            Ok(format!("${}", bind_types.len()))
        }
        MutationExpressionKind::TypedNull => Ok(format!("CAST(NULL AS {})", value_type.name())),
        _ => Err(plan_invariant(
            "unknown future mutation expression kinds are unsupported",
        )),
    }
}

fn parameter_placeholder(
    parameter: ParameterId,
    value_type: Type,
    arguments: &BTreeMap<ParameterId, BindValue>,
    bind_types: &mut Vec<Type>,
    binds: &mut Vec<BindValue>,
    parameter_placeholders: &mut BTreeMap<ParameterId, usize>,
) -> Result<String, PostgresKernelError> {
    if let Some(placeholder) = parameter_placeholders.get(&parameter).copied() {
        return Ok(format!("${placeholder}"));
    }
    let value = arguments.get(&parameter).ok_or_else(|| {
        plan_invariant("validated parameter expression must have one runtime argument")
    })?;
    binds.push(value.clone());
    bind_types.push(value_type);
    let placeholder = bind_types.len();
    parameter_placeholders.insert(parameter, placeholder);
    Ok(format!("${placeholder}"))
}

fn lower_update_with_context(
    context: &orna_core::revision::CatalogueHashContext,
    plan: &ServerMutationPlan,
    arguments: &BTreeMap<ParameterId, BindValue>,
) -> Result<LoweredMutation, PostgresKernelError> {
    let ServerMutationOperation::Update { selector } = plan.operation() else {
        return Err(plan_invariant("UPDATE execution requires an UPDATE plan"));
    };
    let mut assignments = Vec::with_capacity(plan.assignments().len());
    let mut bind_types = Vec::new();
    let mut binds = Vec::new();
    let mut parameter_placeholders = BTreeMap::new();
    for assignment in plan.assignments() {
        let value = lower_assignment_expression(
            context,
            assignment.expression(),
            arguments,
            &mut bind_types,
            &mut binds,
            &mut parameter_placeholders,
        )?;
        assignments.push(format!("{} = {value}", field_name(assignment.field())));
    }
    let selector_placeholder = parameter_placeholder(
        selector.parameter(),
        Type::BYTEA,
        arguments,
        &mut bind_types,
        &mut binds,
        &mut parameter_placeholders,
    )?;
    let sql = format!(
        "UPDATE {DATA_SCHEMA}.{} SET {} WHERE {OBJECT_ID_COLUMN} = {selector_placeholder} RETURNING {OBJECT_ID_COLUMN} AS c0",
        relation_name(plan.target()),
        assignments.join(", "),
    );
    if sql.len() > SQL_LIMIT {
        return Err(server_error(ServerInsertError::ComplexityLimit {
            category: "saved function complexity",
            maximum: SQL_LIMIT,
        }));
    }
    Ok(LoweredMutation {
        sql,
        bind_types,
        binds,
    })
}

#[cfg(test)]
fn lower_update(
    plan: &ServerMutationPlan,
    arguments: &BTreeMap<ParameterId, BindValue>,
) -> Result<LoweredMutation, PostgresKernelError> {
    lower_update_with_context(
        &orna_core::revision::CatalogueHashContext::version_one(),
        plan,
        arguments,
    )
}

fn lower_delete(
    plan: &ServerDeletePlan,
    arguments: &BTreeMap<ParameterId, BindValue>,
) -> Result<LoweredMutation, PostgresKernelError> {
    let selector = plan.selector();
    let value = arguments.get(&selector.parameter()).ok_or_else(|| {
        plan_invariant("validated DELETE selector must have one runtime argument")
    })?;
    let sql = format!(
        "DELETE FROM {DATA_SCHEMA}.{} WHERE {OBJECT_ID_COLUMN} = $1 RETURNING {OBJECT_ID_COLUMN} AS c0",
        relation_name(plan.target()),
    );
    if sql.len() > SQL_LIMIT {
        return Err(server_error(ServerMutationError::ComplexityLimit {
            category: "saved function complexity",
            maximum: SQL_LIMIT,
        }));
    }
    Ok(LoweredMutation {
        sql,
        bind_types: vec![Type::BYTEA],
        binds: vec![value.clone()],
    })
}

fn validate_prepared_result(
    statement: &Statement,
    operation: &'static str,
) -> Result<(), PostgresKernelError> {
    let [column] = statement.columns() else {
        return Err(server_error(ServerInsertError::PreparedResult {
            rule: match operation {
                "INSERT" => "prepared INSERT must return exactly one column",
                "UPDATE" => "prepared UPDATE must return exactly one column",
                "DELETE" => "prepared DELETE must return exactly one column",
                _ => "prepared mutation must return exactly one column",
            },
        }));
    };
    if column.name() != "c0" || *column.type_() != Type::BYTEA {
        return Err(server_error(ServerInsertError::PreparedResult {
            rule: match operation {
                "INSERT" => "prepared INSERT must return one BYTEA column named c0",
                "UPDATE" => "prepared UPDATE must return one BYTEA column named c0",
                "DELETE" => "prepared DELETE must return one BYTEA column named c0",
                _ => "prepared mutation must return one BYTEA column named c0",
            },
        }));
    }
    Ok(())
}

async fn execute_insert(
    transaction: &Transaction<'_>,
    statement: &Statement,
    binds: Vec<BindValue>,
    object: ObjectId,
    unique_references: &UniqueReferenceConstraints,
) -> Result<(), PostgresKernelError> {
    let object_bytes = object.to_bytes().to_vec();
    let mut parameters = Vec::<&(dyn ToSql + Sync)>::with_capacity(binds.len() + 1);
    parameters.push(&object_bytes);
    parameters.extend(binds.iter().map(BindValue::as_to_sql));
    let rows = transaction
        .query(statement, &parameters)
        .await
        .map_err(|source| server_error(mutation_database_error(source, unique_references)))?;
    let [row] = rows.as_slice() else {
        return Err(server_error(ServerInsertError::ValueInvariant {
            rule: "INSERT must return exactly one row",
        }));
    };
    let returned = row
        .try_get::<usize, Vec<u8>>(0)
        .map_err(|source| server_error(ServerInsertError::RowDecode { source }))?;
    let returned: [u8; 16] = returned.try_into().map_err(|_| {
        server_error(ServerInsertError::ValueInvariant {
            rule: "returned object identity must contain exactly 16 bytes",
        })
    })?;
    if ObjectId::from_bytes(returned) != object {
        return Err(server_error(ServerInsertError::ValueInvariant {
            rule: "returned object identity must equal the allocated identity",
        }));
    }
    Ok(())
}

async fn execute_update(
    transaction: &Transaction<'_>,
    statement: &Statement,
    binds: Vec<BindValue>,
    selector: ObjectId,
    unique_references: &UniqueReferenceConstraints,
) -> Result<bool, PostgresKernelError> {
    let parameters = binds.iter().map(BindValue::as_to_sql).collect::<Vec<_>>();
    let rows = transaction
        .query(statement, &parameters)
        .await
        .map_err(|source| server_error(mutation_database_error(source, unique_references)))?;
    decode_selected_result(&rows, selector, "UPDATE")
}

async fn execute_delete(
    transaction: &Transaction<'_>,
    statement: &Statement,
    binds: Vec<BindValue>,
    context: ServerDeleteContext,
    target: TypeId,
    selector: ObjectId,
) -> Result<bool, PostgresKernelError> {
    let parameters = binds.iter().map(BindValue::as_to_sql).collect::<Vec<_>>();
    let rows = transaction
        .query(statement, &parameters)
        .await
        .map_err(|source| {
            if delete_commit_failure(
                source
                    .as_db_error()
                    .map(tokio_postgres::error::DbError::code),
            ) == DeleteCommitFailure::Restricted
            {
                delete_error(ServerDeleteError::DeleteRestricted {
                    context,
                    target,
                    selector,
                    source,
                })
            } else {
                server_error(ServerMutationError::Database { source })
            }
        })?;
    decode_selected_result(&rows, selector, "DELETE")
}

fn decode_selected_result(
    rows: &[Row],
    selector: ObjectId,
    operation: &'static str,
) -> Result<bool, PostgresKernelError> {
    let [row] = rows else {
        if rows.is_empty() {
            return Ok(false);
        }
        return Err(server_error(ServerInsertError::ValueInvariant {
            rule: match operation {
                "UPDATE" => "UPDATE must return at most one row",
                "DELETE" => "DELETE must return at most one row",
                _ => "identity-selected mutation must return at most one row",
            },
        }));
    };
    let returned = row
        .try_get::<usize, Vec<u8>>(0)
        .map_err(|source| server_error(ServerInsertError::RowDecode { source }))?;
    let returned: [u8; 16] = returned.try_into().map_err(|_| {
        server_error(ServerInsertError::ValueInvariant {
            rule: "returned object identity must contain exactly 16 bytes",
        })
    })?;
    if ObjectId::from_bytes(returned) != selector {
        return Err(server_error(ServerInsertError::ValueInvariant {
            rule: match operation {
                "UPDATE" => "updated object identity must equal the selected identity",
                "DELETE" => "deleted object identity must equal the selected identity",
                _ => "returned object identity must equal the selected identity",
            },
        }));
    }
    Ok(true)
}

fn server_error(error: ServerInsertError) -> PostgresKernelError {
    PostgresKernelError::ServerInsert(error)
}

fn update_error(error: ServerUpdateError) -> PostgresKernelError {
    PostgresKernelError::ServerUpdate(error)
}

fn delete_error(error: ServerDeleteError) -> PostgresKernelError {
    PostgresKernelError::ServerDelete(error)
}

fn update_unavailable(error: PostgresKernelError) -> PostgresKernelError {
    update_error(ServerUpdateError::Unavailable {
        source: Box::new(error),
    })
}

fn delete_unavailable(error: PostgresKernelError) -> PostgresKernelError {
    delete_error(ServerDeleteError::Unavailable {
        source: Box::new(error),
    })
}

fn mutation_kernel_error(
    operation: MutationExecutionKind,
    error: PostgresKernelError,
) -> PostgresKernelError {
    match operation {
        MutationExecutionKind::Insert => kernel_error(error),
        MutationExecutionKind::Update => update_unavailable(error),
        MutationExecutionKind::Delete => delete_unavailable(error),
    }
}

fn pre_transaction_mutation_error(
    operation: MutationExecutionKind,
    error: PostgresKernelError,
) -> PostgresKernelError {
    match operation {
        MutationExecutionKind::Insert => pre_transaction_error(error),
        MutationExecutionKind::Update => update_unavailable(error),
        MutationExecutionKind::Delete => delete_unavailable(error),
    }
}

fn update_not_committed(
    context: ServerUpdateContext,
    error: PostgresKernelError,
) -> PostgresKernelError {
    let source = match error {
        PostgresKernelError::ServerInsert(source) => source,
        PostgresKernelError::Database(source) => ServerMutationError::Database { source },
        error => ServerMutationError::Kernel {
            source: Box::new(error),
        },
    };
    update_error(ServerUpdateError::NotCommitted {
        context,
        source: Box::new(source),
    })
}

fn delete_not_committed(
    context: ServerDeleteContext,
    error: PostgresKernelError,
) -> PostgresKernelError {
    let source = match error {
        PostgresKernelError::ServerDelete(error) => {
            return PostgresKernelError::ServerDelete(error);
        }
        PostgresKernelError::ServerInsert(source) => source,
        PostgresKernelError::Database(source) => ServerMutationError::Database { source },
        error => ServerMutationError::Kernel {
            source: Box::new(error),
        },
    };
    delete_error(ServerDeleteError::NotCommitted {
        context,
        source: Box::new(source),
    })
}

fn kernel_error(error: PostgresKernelError) -> PostgresKernelError {
    match error {
        PostgresKernelError::ServerInsert(_) => error,
        error => server_error(ServerInsertError::Kernel {
            source: Box::new(error),
        }),
    }
}

fn pre_transaction_error(error: PostgresKernelError) -> PostgresKernelError {
    match error {
        PostgresKernelError::Database(source) => {
            server_error(ServerInsertError::Database { source })
        }
        error => kernel_error(error),
    }
}

fn not_committed(context: ServerInsertContext, error: PostgresKernelError) -> PostgresKernelError {
    let source = match error {
        PostgresKernelError::ServerInsert(source) => source,
        PostgresKernelError::Database(source) => ServerInsertError::Database { source },
        error => ServerInsertError::Kernel {
            source: Box::new(error),
        },
    };
    server_error(ServerInsertError::NotCommitted {
        context,
        source: Box::new(source),
    })
}

fn artifact_error(function: FunctionId, rule: &'static str) -> PostgresKernelError {
    server_error(ServerInsertError::Artifact { function, rule })
}

fn plan_invariant(rule: &'static str) -> PostgresKernelError {
    server_error(ServerInsertError::PlanInvariant { rule })
}

fn argument_error(parameter: Option<ParameterId>, rule: &'static str) -> PostgresKernelError {
    server_error(ServerInsertError::Argument { parameter, rule })
}

#[cfg(test)]
mod tests {
    use orna_artifact::server_mutation_plan::{FieldAssignment, MutationExpression};
    use orna_core::{
        CatalogueRevisionId, ExpressionId, FieldId, SchemaId, SourceRevisionId,
        catalogue::{
            FieldDefinition, FunctionReturnColumnDefinition, ParameterDefinition,
            QualifiedSemanticName, SchemaDefinition,
        },
        types::StandardScalar,
        value::RuntimeFloat,
    };

    use super::*;

    const TARGET: TypeId = TypeId::from_bytes([0x10; 16]);
    const OTHER: TypeId = TypeId::from_bytes([0x20; 16]);
    const MISSING: TypeId = TypeId::from_bytes([0x21; 16]);
    const FUNCTION: FunctionId = FunctionId::from_bytes([0x30; 16]);
    const OTHER_FUNCTION: FunctionId = FunctionId::from_bytes([0x31; 16]);
    const REVISION: FunctionRevisionId = FunctionRevisionId::from_bytes([0x32; 16]);
    const FIELD_TITLE: FieldId = FieldId::from_bytes([0x41; 16]);
    const FIELD_ENABLED: FieldId = FieldId::from_bytes([0x42; 16]);
    const FIELD_COUNT: FieldId = FieldId::from_bytes([0x43; 16]);
    const FIELD_OWNER: FieldId = FieldId::from_bytes([0x44; 16]);
    const FIELD_NOTE: FieldId = FieldId::from_bytes([0x45; 16]);
    const PARAMETER_TITLE: ParameterId = ParameterId::from_bytes([0x51; 16]);
    const PARAMETER_OWNER: ParameterId = ParameterId::from_bytes([0x52; 16]);
    const PARAMETER_SELECTOR: ParameterId = ParameterId::from_bytes([0x53; 16]);
    const OBJECT: ObjectId = ObjectId::from_bytes([0x61; 16]);
    const SELECTED_OBJECT: ObjectId = ObjectId::from_bytes([0x62; 16]);

    fn name(parts: &[&str]) -> QualifiedSemanticName {
        QualifiedSemanticName::new(parts.iter().copied()).unwrap()
    }

    fn field(
        id: FieldId,
        semantic_name: &str,
        ordinal: u32,
        resolved_type: ResolvedType,
        nullable: bool,
    ) -> FieldDefinition {
        FieldDefinition::new(
            id,
            semantic_name,
            ordinal,
            resolved_type,
            nullable,
            false,
            None,
            None,
        )
    }

    fn target_fields(reference_target: TypeId) -> Vec<FieldDefinition> {
        vec![
            field(
                FIELD_TITLE,
                "semantic_title",
                0,
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                false,
            ),
            field(
                FIELD_ENABLED,
                "semantic_enabled",
                1,
                ResolvedType::scalar(StandardScalar::Boolean),
                false,
            ),
            field(
                FIELD_COUNT,
                "semantic_count",
                2,
                ResolvedType::scalar(StandardScalar::Integer),
                true,
            ),
            field(
                FIELD_OWNER,
                "semantic_owner",
                3,
                ResolvedType::reference(reference_target),
                true,
            ),
            field(
                FIELD_NOTE,
                "semantic_note",
                4,
                ResolvedType::scalar(StandardScalar::BinaryLargeObject),
                true,
            ),
        ]
    }

    fn object_types(
        fields: Vec<FieldDefinition>,
        include_other: bool,
    ) -> Vec<ObjectTypeDefinition> {
        let mut objects = vec![ObjectTypeDefinition::new(
            TARGET,
            name(&["test", "semantic_target"]),
            fields,
        )];
        if include_other {
            objects.push(ObjectTypeDefinition::new(
                OTHER,
                name(&["test", "semantic_other"]),
                Vec::new(),
            ));
        }
        objects
    }

    fn catalogue(
        fields: Vec<FieldDefinition>,
        include_other: bool,
        functions: Vec<FunctionDefinition>,
    ) -> CatalogueSnapshot {
        CatalogueSnapshot::new_with_functions(
            CatalogueRevisionId::from_bytes([0x01; 16]),
            vec![SchemaDefinition::new(
                SchemaId::from_bytes([0x02; 16]),
                name(&["test"]),
            )],
            object_types(fields, include_other),
            functions,
        )
        .unwrap()
    }

    fn parameters(reference_target: TypeId) -> Vec<ParameterDefinition> {
        vec![
            ParameterDefinition::new(
                PARAMETER_TITLE,
                "semantic_title_parameter",
                0,
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                None,
            ),
            ParameterDefinition::new(
                PARAMETER_OWNER,
                "semantic_owner_parameter",
                1,
                ResolvedType::reference(reference_target),
                None,
            ),
        ]
    }

    fn function(
        domain: FunctionDomain,
        parameters: Vec<ParameterDefinition>,
        return_type: FunctionReturn,
        security: FunctionSecurity,
        transaction: Option<FunctionTransaction>,
        volatility: FunctionVolatility,
    ) -> FunctionDefinition {
        FunctionDefinition::new(
            FUNCTION,
            name(&["test", "semantic_insert"]),
            domain,
            parameters,
            return_type,
            REVISION,
            security,
            transaction,
            volatility,
        )
    }

    fn rows_reference(target: TypeId) -> FunctionReturn {
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "semantic_created",
            0,
            ResolvedType::reference(target),
        )])
    }

    fn valid_function() -> FunctionDefinition {
        function(
            FunctionDomain::Server,
            parameters(OTHER),
            rows_reference(TARGET),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        )
    }

    fn valid_plan() -> ServerMutationPlan {
        ServerMutationPlan::new_insert(
            TARGET,
            [
                FieldAssignment::new(
                    TARGET,
                    FIELD_TITLE,
                    MutationExpression::parameter(
                        FUNCTION,
                        PARAMETER_TITLE,
                        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    )
                    .unwrap(),
                ),
                FieldAssignment::new(
                    TARGET,
                    FIELD_ENABLED,
                    MutationExpression::boolean_literal(true),
                ),
                FieldAssignment::new(
                    TARGET,
                    FIELD_COUNT,
                    MutationExpression::typed_null(ResolvedType::scalar(StandardScalar::Integer))
                        .unwrap(),
                ),
                FieldAssignment::new(
                    TARGET,
                    FIELD_OWNER,
                    MutationExpression::parameter(
                        FUNCTION,
                        PARAMETER_OWNER,
                        ResolvedType::reference(OTHER),
                    )
                    .unwrap(),
                ),
            ],
            TARGET,
        )
        .unwrap()
    }

    fn valid_arguments() -> Vec<FunctionArgument> {
        vec![
            FunctionArgument::new(
                PARAMETER_OWNER,
                RuntimeValue::Reference {
                    target: OTHER,
                    object: OBJECT,
                },
            )
            .unwrap(),
            FunctionArgument::new(PARAMETER_TITLE, RuntimeValue::Text(String::from("title")))
                .unwrap(),
        ]
    }

    fn valid_update_function() -> FunctionDefinition {
        let mut declared_parameters = vec![ParameterDefinition::new(
            PARAMETER_SELECTOR,
            "semantic_selector_parameter",
            0,
            ResolvedType::reference(TARGET),
            None,
        )];
        declared_parameters.extend(parameters(OTHER).into_iter().enumerate().map(
            |(index, parameter)| {
                ParameterDefinition::new(
                    parameter.id(),
                    parameter.name(),
                    u32::try_from(index + 1).unwrap(),
                    parameter.resolved_type(),
                    parameter.default_expression(),
                )
            },
        ));
        function(
            FunctionDomain::Server,
            declared_parameters,
            rows_reference(TARGET),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        )
    }

    fn valid_update_plan() -> ServerMutationPlan {
        ServerMutationPlan::new_update(
            TARGET,
            server_mutation_plan::MutationSelector::new(FUNCTION, PARAMETER_SELECTOR),
            [
                FieldAssignment::new(
                    TARGET,
                    FIELD_TITLE,
                    MutationExpression::parameter(
                        FUNCTION,
                        PARAMETER_TITLE,
                        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    )
                    .unwrap(),
                ),
                FieldAssignment::new(
                    TARGET,
                    FIELD_OWNER,
                    MutationExpression::parameter(
                        FUNCTION,
                        PARAMETER_OWNER,
                        ResolvedType::reference(OTHER),
                    )
                    .unwrap(),
                ),
            ],
            TARGET,
        )
        .unwrap()
    }

    fn valid_update_arguments() -> Vec<FunctionArgument> {
        let mut arguments = valid_arguments();
        arguments.push(
            FunctionArgument::new(
                PARAMETER_SELECTOR,
                RuntimeValue::Reference {
                    target: TARGET,
                    object: SELECTED_OBJECT,
                },
            )
            .unwrap(),
        );
        arguments
    }

    fn rows_boolean() -> FunctionReturn {
        FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
            "semantic_deleted",
            0,
            ResolvedType::scalar(StandardScalar::Boolean),
        )])
    }

    fn valid_delete_function() -> FunctionDefinition {
        function(
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                PARAMETER_SELECTOR,
                "semantic_selector_parameter",
                0,
                ResolvedType::reference(TARGET),
                None,
            )],
            rows_boolean(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        )
    }

    fn valid_delete_plan() -> ServerDeletePlan {
        ServerDeletePlan::new(TARGET, MutationSelector::new(FUNCTION, PARAMETER_SELECTOR))
    }

    fn valid_delete_arguments() -> Vec<FunctionArgument> {
        vec![
            FunctionArgument::new(
                PARAMETER_SELECTOR,
                RuntimeValue::Reference {
                    target: TARGET,
                    object: SELECTED_OBJECT,
                },
            )
            .unwrap(),
        ]
    }

    fn retained_standard_context() -> orna_core::revision::CatalogueHashContext {
        orna_core::revision::CatalogueHashContext::version_two(
            orna_standard::verify_standard_library_snapshot(
                orna_standard::retained_standard_library_snapshot().unwrap(),
            )
            .unwrap(),
        )
    }

    fn value_target_fields(reference_target: TypeId) -> Vec<FieldDefinition> {
        let mut fields = target_fields(reference_target);
        fields[0] = field(
            FIELD_TITLE,
            "semantic_title",
            0,
            ResolvedType::value(orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID),
            false,
        );
        fields[1] = field(
            FIELD_ENABLED,
            "semantic_enabled",
            1,
            ResolvedType::value(orna_standard::BOOLEAN_TYPE_ID),
            false,
        );
        fields
    }

    fn value_insert_function() -> FunctionDefinition {
        let mut declared_parameters = parameters(OTHER);
        declared_parameters[0] = ParameterDefinition::new(
            PARAMETER_TITLE,
            "semantic_title_parameter",
            0,
            ResolvedType::value(orna_standard::CHARACTER_LARGE_OBJECT_TYPE_ID),
            None,
        );
        function(
            FunctionDomain::Server,
            declared_parameters,
            rows_reference(TARGET),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        )
    }

    fn value_insert_plan() -> ServerMutationPlan {
        ServerMutationPlan::new_insert(
            TARGET,
            [
                FieldAssignment::new(
                    TARGET,
                    FIELD_TITLE,
                    MutationExpression::parameter(
                        FUNCTION,
                        PARAMETER_TITLE,
                        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    )
                    .unwrap(),
                ),
                FieldAssignment::new(
                    TARGET,
                    FIELD_ENABLED,
                    MutationExpression::boolean_literal(true),
                ),
                FieldAssignment::new(
                    TARGET,
                    FIELD_COUNT,
                    MutationExpression::typed_null(ResolvedType::scalar(StandardScalar::Integer))
                        .unwrap(),
                ),
                FieldAssignment::new(
                    TARGET,
                    FIELD_OWNER,
                    MutationExpression::parameter(
                        FUNCTION,
                        PARAMETER_OWNER,
                        ResolvedType::reference(OTHER),
                    )
                    .unwrap(),
                ),
            ],
            TARGET,
        )
        .unwrap()
    }

    fn value_delete_function() -> FunctionDefinition {
        function(
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                PARAMETER_SELECTOR,
                "semantic_selector_parameter",
                0,
                ResolvedType::reference(TARGET),
                None,
            )],
            FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                "semantic_deleted",
                0,
                ResolvedType::value(orna_standard::BOOLEAN_TYPE_ID),
            )]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        )
    }

    #[test]
    fn verified_value_insert_preserves_legacy_bind_shapes_and_sql() {
        let legacy_catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let legacy_function = valid_function();
        let legacy_plan = valid_plan();
        validate_plan(&legacy_catalogue, &legacy_function, TARGET, &legacy_plan).unwrap();
        let legacy_arguments =
            validate_arguments(&legacy_catalogue, &legacy_function, &valid_arguments()).unwrap();
        let legacy_lowered = lower_insert(&legacy_plan, &legacy_arguments).unwrap();

        let context = retained_standard_context();
        let value_catalogue = catalogue(value_target_fields(OTHER), true, Vec::new());
        let value_function = value_insert_function();
        let value_plan = value_insert_plan();
        validate_function_signature_for_context(
            &context,
            &value_catalogue,
            &value_function,
            MutationExecutionKind::Insert,
        )
        .unwrap();
        validate_plan_for_context(
            &context,
            &value_catalogue,
            &value_function,
            TARGET,
            &value_plan,
            MutationExecutionKind::Insert,
        )
        .unwrap();
        let value_arguments = validate_arguments_with_context(
            &context,
            &value_catalogue,
            &value_function,
            &valid_arguments(),
        )
        .unwrap();
        let value_lowered =
            lower_insert_with_context(&context, &value_plan, &value_arguments).unwrap();

        assert_eq!(value_lowered.sql, legacy_lowered.sql);
        assert_eq!(value_lowered.bind_types, legacy_lowered.bind_types);
        assert_eq!(value_lowered.binds, legacy_lowered.binds);
    }

    #[test]
    fn verified_value_update_preserves_bind_shapes_and_exact_selector_sql() {
        let legacy_catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let legacy_function = valid_update_function();
        let legacy_plan = valid_update_plan();
        validate_plan_for_operation(
            &legacy_catalogue,
            &legacy_function,
            TARGET,
            &legacy_plan,
            MutationExecutionKind::Update,
        )
        .unwrap();
        let legacy_arguments = validate_arguments(
            &legacy_catalogue,
            &legacy_function,
            &valid_update_arguments(),
        )
        .unwrap();
        let legacy_lowered = lower_update(&legacy_plan, &legacy_arguments).unwrap();

        let context = retained_standard_context();
        let value_catalogue = catalogue(value_target_fields(OTHER), true, Vec::new());
        let value_function = valid_update_function();
        let value_plan = valid_update_plan();
        validate_plan_for_context(
            &context,
            &value_catalogue,
            &value_function,
            TARGET,
            &value_plan,
            MutationExecutionKind::Update,
        )
        .unwrap();
        let value_arguments = validate_arguments_with_context(
            &context,
            &value_catalogue,
            &value_function,
            &valid_update_arguments(),
        )
        .unwrap();
        let value_lowered =
            lower_update_with_context(&context, &value_plan, &value_arguments).unwrap();

        assert_eq!(value_lowered.sql, legacy_lowered.sql);
        assert_eq!(value_lowered.bind_types, legacy_lowered.bind_types);
        assert_eq!(value_lowered.binds, legacy_lowered.binds);
    }

    #[test]
    fn verified_value_delete_boolean_return_keeps_the_legacy_result_shape() {
        let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let legacy =
            validate_delete_function_signature(&catalogue, &valid_delete_function()).unwrap();
        let context = retained_standard_context();
        let value = validate_delete_function_signature_with_context(
            &context,
            &catalogue,
            &value_delete_function(),
        )
        .unwrap();

        assert_eq!(value, legacy);
        assert_eq!(
            value.resolved_type(),
            ResolvedType::scalar(StandardScalar::Boolean)
        );
    }

    #[test]
    fn verified_value_with_unsupported_contract_keeps_the_existing_signature_rule() {
        let mut declared_parameters = parameters(OTHER);
        declared_parameters[0] = ParameterDefinition::new(
            PARAMETER_TITLE,
            "semantic_title_parameter",
            0,
            ResolvedType::value(orna_standard::DECIMAL_TYPE_ID),
            None,
        );
        let function = function(
            FunctionDomain::Server,
            declared_parameters,
            rows_reference(TARGET),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        );
        let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let error = validate_function_signature_for_context(
            &retained_standard_context(),
            &catalogue,
            &function,
            MutationExecutionKind::Insert,
        )
        .unwrap_err();

        match expect_insert_error(error) {
            ServerInsertError::FunctionSignature { function, rule } => {
                assert_eq!(function, FUNCTION);
                assert_eq!(
                    rule,
                    "every INSERT SERVER function parameter must use a supported active type"
                );
            }
            other => panic!("unexpected mutation error: {other:?}"),
        }
    }

    fn expect_insert_error(error: PostgresKernelError) -> ServerInsertError {
        let PostgresKernelError::ServerInsert(error) = error else {
            panic!("expected typed SERVER INSERT error");
        };
        error
    }

    fn expect_update_error(error: PostgresKernelError) -> ServerUpdateError {
        let PostgresKernelError::ServerUpdate(error) = error else {
            panic!("expected typed SERVER UPDATE error");
        };
        error
    }

    fn expect_delete_error(error: PostgresKernelError) -> ServerDeleteError {
        let PostgresKernelError::ServerDelete(error) = error else {
            panic!("expected typed SERVER DELETE error");
        };
        error
    }

    #[test]
    fn context_and_result_expose_only_stable_execution_facts() {
        let pair = RevisionPair::new(
            SourceRevisionId::from_bytes([0x71; 16]),
            CatalogueRevisionId::from_bytes([0x72; 16]),
        );
        let context = ServerInsertContext::new(pair, FUNCTION, REVISION);
        let result = ServerInsertResult::new(
            context,
            TARGET,
            OBJECT,
            ResultColumn::new("semantic_created", ResolvedType::reference(TARGET), false).unwrap(),
        )
        .unwrap();

        assert_eq!(context.pair(), pair);
        assert_eq!(context.function(), FUNCTION);
        assert_eq!(context.function_revision(), REVISION);
        assert_eq!(result.context(), context);
        assert_eq!(result.pair(), pair);
        assert_eq!(result.function(), FUNCTION);
        assert_eq!(result.function_revision(), REVISION);
        assert_eq!(result.target(), TARGET);
        assert_eq!(result.object(), OBJECT);
        assert_eq!(result.rows().columns().len(), 1);
        assert_eq!(result.rows().columns()[0].name(), "semantic_created");
        assert_eq!(
            result.rows().columns()[0].resolved_type(),
            ResolvedType::reference(TARGET),
        );
        assert!(!result.rows().columns()[0].nullable());
        assert_eq!(
            result.rows().rows()[0].values(),
            &[RuntimeValue::Reference {
                target: TARGET,
                object: OBJECT,
            }],
        );
    }

    #[test]
    fn update_result_distinguishes_absent_and_matched_objects() {
        let pair = RevisionPair::new(
            SourceRevisionId::from_bytes([0x71; 16]),
            CatalogueRevisionId::from_bytes([0x72; 16]),
        );
        let context = ServerUpdateContext::new(pair, FUNCTION, REVISION);
        let column = || {
            ResultColumn::new("semantic_updated", ResolvedType::reference(TARGET), false).unwrap()
        };
        let absent =
            ServerUpdateResult::new(context, TARGET, SELECTED_OBJECT, false, column()).unwrap();
        let matched =
            ServerUpdateResult::new(context, TARGET, SELECTED_OBJECT, true, column()).unwrap();

        assert_eq!(absent.context(), context);
        assert_eq!(absent.pair(), pair);
        assert_eq!(absent.function(), FUNCTION);
        assert_eq!(absent.function_revision(), REVISION);
        assert_eq!(absent.target(), TARGET);
        assert_eq!(absent.selector(), SELECTED_OBJECT);
        assert!(!absent.matched());
        assert!(absent.rows().rows().is_empty());
        assert_eq!(absent.rows().columns(), matched.rows().columns());
        assert!(matched.matched());
        assert_eq!(
            matched.rows().rows()[0].values(),
            &[RuntimeValue::Reference {
                target: TARGET,
                object: SELECTED_OBJECT,
            }],
        );
    }

    #[test]
    fn delete_result_distinguishes_absent_and_deleted_objects() {
        let pair = RevisionPair::new(
            SourceRevisionId::from_bytes([0x71; 16]),
            CatalogueRevisionId::from_bytes([0x72; 16]),
        );
        let context = ServerDeleteContext::new(pair, FUNCTION, REVISION);
        let column = || {
            ResultColumn::new(
                "semantic_deleted",
                ResolvedType::scalar(StandardScalar::Boolean),
                false,
            )
            .unwrap()
        };
        let absent =
            ServerDeleteResult::new(context, TARGET, SELECTED_OBJECT, false, column()).unwrap();
        let deleted =
            ServerDeleteResult::new(context, TARGET, SELECTED_OBJECT, true, column()).unwrap();

        assert_eq!(absent.context(), context);
        assert_eq!(absent.pair(), pair);
        assert_eq!(absent.function(), FUNCTION);
        assert_eq!(absent.function_revision(), REVISION);
        assert_eq!(absent.target(), TARGET);
        assert_eq!(absent.selector(), SELECTED_OBJECT);
        assert!(!absent.matched());
        assert!(absent.rows().rows().is_empty());
        assert_eq!(absent.rows().columns(), deleted.rows().columns());
        assert_eq!(deleted.rows().columns()[0].name(), "semantic_deleted");
        assert_eq!(
            deleted.rows().columns()[0].resolved_type(),
            ResolvedType::scalar(StandardScalar::Boolean),
        );
        assert!(!deleted.rows().columns()[0].nullable());
        assert!(deleted.matched());
        assert_eq!(
            deleted.rows().rows()[0].values(),
            &[RuntimeValue::Boolean(true)],
        );
    }

    #[test]
    fn delete_metadata_signature_plan_selector_and_references_are_exact() {
        let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let function = valid_delete_function();
        let plan = valid_delete_plan();

        validate_artifact_metadata_for_operation(
            FUNCTION,
            ExecutableArtifactKind::Server,
            server_mutation_plan::FORMAT_IDENTITY,
            server_mutation_plan::DELETE_FORMAT_VERSION,
            server_mutation_plan::LANGUAGE_VERSION_IDENTITY,
            MutationExecutionKind::Delete,
        )
        .unwrap();
        for version in [
            server_mutation_plan::INSERT_FORMAT_VERSION,
            server_mutation_plan::UPDATE_FORMAT_VERSION,
        ] {
            assert!(
                validate_artifact_metadata_for_operation(
                    FUNCTION,
                    ExecutableArtifactKind::Server,
                    server_mutation_plan::FORMAT_IDENTITY,
                    version,
                    server_mutation_plan::LANGUAGE_VERSION_IDENTITY,
                    MutationExecutionKind::Delete,
                )
                .is_err()
            );
        }

        let column = validate_delete_function_signature(&catalogue, &function).unwrap();
        assert_eq!(column.name(), "semantic_deleted");
        assert_eq!(
            column.resolved_type(),
            ResolvedType::scalar(StandardScalar::Boolean),
        );
        assert!(!column.nullable());
        assert_eq!(
            validate_delete_plan(&catalogue, &function, &plan)
                .unwrap()
                .id(),
            TARGET
        );
        assert_eq!(
            plan.format_version(),
            server_mutation_plan::DELETE_FORMAT_VERSION
        );
        assert_eq!(plan.target(), TARGET);
        assert_eq!(
            plan.selector(),
            MutationSelector::new(FUNCTION, PARAMETER_SELECTOR),
        );
        assert_eq!(
            selector_argument_object(plan.target(), plan.selector(), &valid_delete_arguments())
                .unwrap(),
            SELECTED_OBJECT,
        );
        assert_eq!(
            expected_delete_body_references(&plan),
            [
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::WriteObject,
                    DefinitionReferenceTarget::ObjectType(TARGET),
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(TARGET),
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::ParameterRead,
                    DefinitionReferenceTarget::Parameter {
                        owner: FUNCTION,
                        parameter: PARAMETER_SELECTOR,
                    },
                ),
            ],
        );
    }

    #[test]
    fn delete_rejects_wrong_result_selector_owner_parameter_and_target() {
        let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let wrong_result = function(
            FunctionDomain::Server,
            valid_delete_function().parameters().to_vec(),
            rows_reference(TARGET),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        );
        assert!(matches!(
            expect_insert_error(
                validate_delete_function_signature(&catalogue, &wrong_result).unwrap_err()
            ),
            ServerMutationError::FunctionSignature { .. },
        ));

        for selector in [
            MutationSelector::new(OTHER_FUNCTION, PARAMETER_SELECTOR),
            MutationSelector::new(FUNCTION, ParameterId::from_bytes([0x54; 16])),
        ] {
            assert!(
                validate_delete_plan(
                    &catalogue,
                    &valid_delete_function(),
                    &ServerDeletePlan::new(TARGET, selector),
                )
                .is_err()
            );
        }

        let wrong_target_function = function(
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                PARAMETER_SELECTOR,
                "selector",
                0,
                ResolvedType::reference(OTHER),
                None,
            )],
            rows_boolean(),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        );
        assert!(
            validate_delete_plan(&catalogue, &wrong_target_function, &valid_delete_plan(),)
                .is_err()
        );
    }

    #[test]
    fn update_metadata_plan_selector_and_omitted_fields_are_exact() {
        let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let function = valid_update_function();
        let plan = valid_update_plan();

        validate_artifact_metadata_for_operation(
            FUNCTION,
            ExecutableArtifactKind::Server,
            server_mutation_plan::FORMAT_IDENTITY,
            server_mutation_plan::UPDATE_FORMAT_VERSION,
            server_mutation_plan::LANGUAGE_VERSION_IDENTITY,
            MutationExecutionKind::Update,
        )
        .unwrap();
        assert!(
            validate_artifact_metadata_for_operation(
                FUNCTION,
                ExecutableArtifactKind::Server,
                server_mutation_plan::FORMAT_IDENTITY,
                server_mutation_plan::INSERT_FORMAT_VERSION,
                server_mutation_plan::LANGUAGE_VERSION_IDENTITY,
                MutationExecutionKind::Update,
            )
            .is_err()
        );
        let returned = validate_function_signature_for_operation(
            &catalogue,
            &function,
            MutationExecutionKind::Update,
        )
        .unwrap();
        let target = validate_plan_for_operation(
            &catalogue,
            &function,
            returned.target,
            &plan,
            MutationExecutionKind::Update,
        )
        .unwrap()
        .target;

        assert_eq!(target.id(), TARGET);
        assert_eq!(plan.format_version(), 2);
        assert_eq!(
            plan.selector(),
            Some(server_mutation_plan::MutationSelector::new(
                FUNCTION,
                PARAMETER_SELECTOR,
            )),
        );
        assert_eq!(plan.assignments().len(), 2);
        assert!(
            target
                .fields()
                .iter()
                .filter(|field| !field.nullable())
                .any(|field| !plan
                    .assignments()
                    .iter()
                    .any(|assignment| assignment.field() == field.id()))
        );
        assert!(
            validate_plan_for_operation(
                &catalogue,
                &function,
                returned.target,
                &plan,
                MutationExecutionKind::Insert,
            )
            .is_err()
        );
    }

    #[test]
    fn update_selector_requires_the_active_exact_target_reference_parameter() {
        let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let missing_parameter = ServerMutationPlan::new_update(
            TARGET,
            server_mutation_plan::MutationSelector::new(
                FUNCTION,
                ParameterId::from_bytes([0x54; 16]),
            ),
            [FieldAssignment::new(
                TARGET,
                FIELD_ENABLED,
                MutationExpression::boolean_literal(true),
            )],
            TARGET,
        )
        .unwrap();
        assert!(
            validate_plan_for_operation(
                &catalogue,
                &valid_update_function(),
                TARGET,
                &missing_parameter,
                MutationExecutionKind::Update,
            )
            .is_err()
        );

        for selector_type in [
            ResolvedType::scalar(StandardScalar::Integer),
            ResolvedType::reference(OTHER),
        ] {
            let function = function(
                FunctionDomain::Server,
                vec![ParameterDefinition::new(
                    PARAMETER_SELECTOR,
                    "selector",
                    0,
                    selector_type,
                    None,
                )],
                rows_reference(TARGET),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::Atomic),
                FunctionVolatility::Volatile,
            );
            let plan = ServerMutationPlan::new_update(
                TARGET,
                server_mutation_plan::MutationSelector::new(FUNCTION, PARAMETER_SELECTOR),
                [FieldAssignment::new(
                    TARGET,
                    FIELD_ENABLED,
                    MutationExpression::boolean_literal(true),
                )],
                TARGET,
            )
            .unwrap();
            assert!(
                validate_plan_for_operation(
                    &catalogue,
                    &function,
                    TARGET,
                    &plan,
                    MutationExecutionKind::Update,
                )
                .is_err()
            );
        }

        let wrong_owner = ServerMutationPlan::new_update(
            TARGET,
            server_mutation_plan::MutationSelector::new(OTHER_FUNCTION, PARAMETER_SELECTOR),
            [FieldAssignment::new(
                TARGET,
                FIELD_ENABLED,
                MutationExpression::boolean_literal(true),
            )],
            TARGET,
        )
        .unwrap();
        assert!(
            validate_plan_for_operation(
                &catalogue,
                &valid_update_function(),
                TARGET,
                &wrong_owner,
                MutationExecutionKind::Update,
            )
            .is_err()
        );
    }

    #[test]
    fn signature_accepts_only_server_invoker_atomic_volatile_rows_ref() {
        let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let returned = validate_function_signature(&catalogue, &valid_function()).unwrap();
        assert_eq!(returned.target, TARGET);
        assert_eq!(returned.column.name(), "semantic_created");
        assert_eq!(
            returned.column.resolved_type(),
            ResolvedType::reference(TARGET),
        );
        assert!(!returned.column.nullable());

        let invalid = [
            function(
                FunctionDomain::Client,
                parameters(OTHER),
                rows_reference(TARGET),
                FunctionSecurity::Invoker,
                None,
                FunctionVolatility::Volatile,
            ),
            function(
                FunctionDomain::Server,
                parameters(OTHER),
                rows_reference(TARGET),
                FunctionSecurity::Definer,
                Some(FunctionTransaction::Atomic),
                FunctionVolatility::Volatile,
            ),
            function(
                FunctionDomain::Server,
                parameters(OTHER),
                rows_reference(TARGET),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::ReadOnly),
                FunctionVolatility::Volatile,
            ),
            function(
                FunctionDomain::Server,
                parameters(OTHER),
                rows_reference(TARGET),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::Atomic),
                FunctionVolatility::Stable,
            ),
            function(
                FunctionDomain::Server,
                parameters(OTHER),
                FunctionReturn::Single(ResolvedType::reference(TARGET)),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::Atomic),
                FunctionVolatility::Volatile,
            ),
            function(
                FunctionDomain::Server,
                parameters(OTHER),
                FunctionReturn::Rows(vec![FunctionReturnColumnDefinition::new(
                    "value",
                    0,
                    ResolvedType::scalar(StandardScalar::Integer),
                )]),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::Atomic),
                FunctionVolatility::Volatile,
            ),
        ];
        for function in invalid {
            assert!(matches!(
                expect_insert_error(
                    validate_function_signature(&catalogue, &function).unwrap_err()
                ),
                ServerInsertError::FunctionSignature { .. },
            ));
        }
    }

    #[test]
    fn signature_rejects_defaults_unsupported_types_and_inactive_references() {
        let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let cases = [
            vec![ParameterDefinition::new(
                PARAMETER_TITLE,
                "value",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                Some(ExpressionId::from_bytes([0x73; 16])),
            )],
            vec![ParameterDefinition::new(
                PARAMETER_TITLE,
                "value",
                0,
                ResolvedType::scalar(StandardScalar::Date),
                None,
            )],
            vec![ParameterDefinition::new(
                PARAMETER_TITLE,
                "value",
                0,
                ResolvedType::reference(MISSING),
                None,
            )],
        ];
        for parameters in cases {
            let candidate = function(
                FunctionDomain::Server,
                parameters,
                rows_reference(TARGET),
                FunctionSecurity::Invoker,
                Some(FunctionTransaction::Atomic),
                FunctionVolatility::Volatile,
            );
            assert!(matches!(
                expect_insert_error(
                    validate_function_signature(&catalogue, &candidate).unwrap_err()
                ),
                ServerInsertError::FunctionSignature { .. },
            ));
        }

        let missing_result = function(
            FunctionDomain::Server,
            Vec::new(),
            rows_reference(MISSING),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        );
        assert!(validate_function_signature(&catalogue, &missing_result).is_err());
    }

    #[test]
    fn artifact_metadata_accepts_only_server_mutation_v1_and_language_v1() {
        assert!(
            validate_artifact_metadata(
                FUNCTION,
                ExecutableArtifactKind::Server,
                server_mutation_plan::FORMAT_IDENTITY,
                server_mutation_plan::FORMAT_VERSION,
                server_mutation_plan::LANGUAGE_VERSION_IDENTITY,
            )
            .is_ok()
        );
        for (kind, format, version, language) in [
            (
                ExecutableArtifactKind::Client,
                server_mutation_plan::FORMAT_IDENTITY,
                1,
                server_mutation_plan::LANGUAGE_VERSION_IDENTITY,
            ),
            (
                ExecutableArtifactKind::Server,
                "orna.server-plan",
                1,
                server_mutation_plan::LANGUAGE_VERSION_IDENTITY,
            ),
            (
                ExecutableArtifactKind::Server,
                server_mutation_plan::FORMAT_IDENTITY,
                2,
                server_mutation_plan::LANGUAGE_VERSION_IDENTITY,
            ),
            (
                ExecutableArtifactKind::Server,
                server_mutation_plan::FORMAT_IDENTITY,
                1,
                "orna.language/2",
            ),
        ] {
            assert!(matches!(
                expect_insert_error(
                    validate_artifact_metadata(FUNCTION, kind, format, version, language)
                        .unwrap_err()
                ),
                ServerInsertError::Artifact { .. },
            ));
        }
        assert!(matches!(
            ServerMutationPlan::decode(b"not a mutation plan"),
            Err(server_mutation_plan::ServerMutationPlanError::InvalidMagic),
        ));
    }

    #[test]
    fn plan_matches_the_active_catalogue_and_allows_omitted_nullable_fields() {
        let function = valid_function();
        let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let target = validate_plan(&catalogue, &function, TARGET, &valid_plan()).unwrap();

        assert_eq!(target.id(), TARGET);
        assert!(
            valid_plan()
                .assignments()
                .iter()
                .all(|assignment| assignment.field() != FIELD_NOTE)
        );
    }

    #[test]
    fn plan_rejects_unknown_fields_type_mismatches_nullability_and_omissions() {
        let function = valid_function();
        let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let cases = [
            ServerMutationPlan::new_insert(
                TARGET,
                [FieldAssignment::new(
                    TARGET,
                    FieldId::from_bytes([0x7a; 16]),
                    MutationExpression::boolean_literal(true),
                )],
                TARGET,
            )
            .unwrap(),
            ServerMutationPlan::new_insert(
                TARGET,
                [FieldAssignment::new(
                    TARGET,
                    FIELD_TITLE,
                    MutationExpression::boolean_literal(true),
                )],
                TARGET,
            )
            .unwrap(),
            ServerMutationPlan::new_insert(
                TARGET,
                [FieldAssignment::new(
                    TARGET,
                    FIELD_TITLE,
                    MutationExpression::typed_null(ResolvedType::scalar(
                        StandardScalar::CharacterLargeObject,
                    ))
                    .unwrap(),
                )],
                TARGET,
            )
            .unwrap(),
            ServerMutationPlan::new_insert(
                TARGET,
                [FieldAssignment::new(
                    TARGET,
                    FIELD_TITLE,
                    MutationExpression::parameter(
                        OTHER_FUNCTION,
                        PARAMETER_TITLE,
                        ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                    )
                    .unwrap(),
                )],
                TARGET,
            )
            .unwrap(),
        ];
        for plan in cases {
            assert!(matches!(
                expect_insert_error(
                    validate_plan(&catalogue, &function, TARGET, &plan).unwrap_err()
                ),
                ServerInsertError::PlanInvariant { .. },
            ));
        }
    }

    #[test]
    fn plan_accepts_only_required_unique_reference_target_fields() {
        let function = valid_function();
        let mut unique_fields = target_fields(OTHER);
        unique_fields[3] = FieldDefinition::new(
            FIELD_OWNER,
            "semantic_owner",
            3,
            ResolvedType::reference(OTHER),
            false,
            true,
            None,
            None,
        );
        let unique_catalogue = catalogue(unique_fields, true, Vec::new());
        let unique_target =
            validate_plan(&unique_catalogue, &function, TARGET, &valid_plan()).unwrap();
        assert_eq!(
            UniqueReferenceConstraints::from_target(unique_target)
                .unwrap()
                .fields,
            [UniqueReferenceConstraint {
                owner: TARGET,
                field: FIELD_OWNER,
                referenced_type: OTHER,
            }]
        );

        let mut invalid_unique_fields = target_fields(OTHER);
        invalid_unique_fields[0] = FieldDefinition::new(
            FIELD_TITLE,
            "semantic_title",
            0,
            ResolvedType::scalar(StandardScalar::CharacterLargeObject),
            false,
            true,
            None,
            None,
        );
        let invalid_unique_catalogue = catalogue(invalid_unique_fields, true, Vec::new());
        assert!(
            validate_plan(&invalid_unique_catalogue, &function, TARGET, &valid_plan()).is_err()
        );

        let mut nullable_unique_fields = target_fields(OTHER);
        nullable_unique_fields[3] = FieldDefinition::new(
            FIELD_OWNER,
            "semantic_owner",
            3,
            ResolvedType::reference(OTHER),
            true,
            true,
            None,
            None,
        );
        let nullable_unique_catalogue = catalogue(nullable_unique_fields, true, Vec::new());
        assert!(
            validate_plan(&nullable_unique_catalogue, &function, TARGET, &valid_plan()).is_err()
        );
    }

    #[test]
    fn plan_rejects_defaults_inactive_references_and_result_mismatch() {
        let function = valid_function();

        let mut default_fields = target_fields(OTHER);
        default_fields[4] = FieldDefinition::new(
            FIELD_NOTE,
            "semantic_note",
            4,
            ResolvedType::scalar(StandardScalar::BinaryLargeObject),
            true,
            false,
            Some(ExpressionId::from_bytes([0x74; 16])),
            None,
        );
        let default_catalogue = catalogue(default_fields, true, Vec::new());
        assert!(validate_plan(&default_catalogue, &function, TARGET, &valid_plan()).is_err());

        let inactive_catalogue = catalogue(target_fields(MISSING), false, Vec::new());
        assert!(validate_plan(&inactive_catalogue, &function, TARGET, &valid_plan()).is_err());
        assert!(
            validate_plan(
                &catalogue(target_fields(OTHER), true, Vec::new()),
                &function,
                OTHER,
                &valid_plan()
            )
            .is_err()
        );
    }

    #[test]
    fn reference_replay_body_is_write_object_fields_parameter_reads_then_returned_ref() {
        let expected = expected_body_references(&valid_plan());
        assert_eq!(
            expected,
            vec![
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::WriteObject,
                    DefinitionReferenceTarget::ObjectType(TARGET),
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::WriteField,
                    DefinitionReferenceTarget::Field {
                        owner: TARGET,
                        field: FIELD_TITLE,
                    },
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::ParameterRead,
                    DefinitionReferenceTarget::Parameter {
                        owner: FUNCTION,
                        parameter: PARAMETER_TITLE,
                    },
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::WriteField,
                    DefinitionReferenceTarget::Field {
                        owner: TARGET,
                        field: FIELD_ENABLED,
                    },
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::WriteField,
                    DefinitionReferenceTarget::Field {
                        owner: TARGET,
                        field: FIELD_COUNT,
                    },
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::WriteField,
                    DefinitionReferenceTarget::Field {
                        owner: TARGET,
                        field: FIELD_OWNER,
                    },
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::ParameterRead,
                    DefinitionReferenceTarget::Parameter {
                        owner: FUNCTION,
                        parameter: PARAMETER_OWNER,
                    },
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(TARGET),
                ),
            ],
        );
    }

    #[test]
    fn update_reference_replay_includes_selector_before_returning() {
        assert_eq!(
            expected_body_references(&valid_update_plan()),
            vec![
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::WriteObject,
                    DefinitionReferenceTarget::ObjectType(TARGET),
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::WriteField,
                    DefinitionReferenceTarget::Field {
                        owner: TARGET,
                        field: FIELD_TITLE,
                    },
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::ParameterRead,
                    DefinitionReferenceTarget::Parameter {
                        owner: FUNCTION,
                        parameter: PARAMETER_TITLE,
                    },
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::WriteField,
                    DefinitionReferenceTarget::Field {
                        owner: TARGET,
                        field: FIELD_OWNER,
                    },
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::ParameterRead,
                    DefinitionReferenceTarget::Parameter {
                        owner: FUNCTION,
                        parameter: PARAMETER_OWNER,
                    },
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(TARGET),
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::ParameterRead,
                    DefinitionReferenceTarget::Parameter {
                        owner: FUNCTION,
                        parameter: PARAMETER_SELECTOR,
                    },
                ),
                ExpectedDefinitionReference::new(
                    DefinitionReferenceKind::ObjectReference,
                    DefinitionReferenceTarget::ObjectType(TARGET),
                ),
            ],
        );
    }

    #[test]
    fn arguments_are_unordered_exact_typed_and_reference_target_checked() {
        let function = valid_function();
        let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let validated = validate_arguments(&catalogue, &function, &valid_arguments()).unwrap();
        assert_eq!(validated.len(), 2);
        assert_eq!(validated[&PARAMETER_TITLE], BindValue::Text("title".into()));
        assert_eq!(
            validated[&PARAMETER_OWNER],
            BindValue::Bytes(OBJECT.to_bytes().to_vec()),
        );

        let duplicate = [
            valid_arguments()[1].clone(),
            valid_arguments()[1].clone(),
            valid_arguments()[0].clone(),
        ];
        assert!(validate_arguments(&catalogue, &function, &duplicate).is_err());
        assert!(validate_arguments(&catalogue, &function, &valid_arguments()[..1]).is_err());
        let unknown = [
            FunctionArgument::new(
                ParameterId::from_bytes([0x75; 16]),
                RuntimeValue::Integer(1),
            )
            .unwrap(),
            valid_arguments()[0].clone(),
            valid_arguments()[1].clone(),
        ];
        assert!(validate_arguments(&catalogue, &function, &unknown).is_err());
        let wrong_scalar = [
            FunctionArgument::new(PARAMETER_TITLE, RuntimeValue::Integer(1)).unwrap(),
            valid_arguments()[0].clone(),
        ];
        assert!(validate_arguments(&catalogue, &function, &wrong_scalar).is_err());
        let wrong_reference = [
            FunctionArgument::new(
                PARAMETER_OWNER,
                RuntimeValue::Reference {
                    target: TARGET,
                    object: OBJECT,
                },
            )
            .unwrap(),
            valid_arguments()[1].clone(),
        ];
        assert!(validate_arguments(&catalogue, &function, &wrong_reference).is_err());
    }

    #[test]
    fn total_variable_argument_payload_is_bounded() {
        let function = valid_function();
        let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let oversized = [
            FunctionArgument::new(
                PARAMETER_TITLE,
                RuntimeValue::Text("x".repeat(VARIABLE_ARGUMENT_PAYLOAD_LIMIT + 1)),
            )
            .unwrap(),
            valid_arguments()[0].clone(),
        ];
        assert!(matches!(
            expect_insert_error(validate_arguments(&catalogue, &function, &oversized).unwrap_err()),
            ServerInsertError::ComplexityLimit {
                category: "total size of text and binary arguments",
                maximum: VARIABLE_ARGUMENT_PAYLOAD_LIMIT,
            },
        ));
    }

    #[test]
    fn exact_argument_validation_accepts_more_parameters_than_the_assignment_limit() {
        let parameter_count = server_mutation_plan::MAX_ASSIGNMENTS as usize + 1;
        let integer_type = ResolvedType::scalar(StandardScalar::Integer);
        let parameters = (0..parameter_count)
            .map(|index| {
                ParameterDefinition::new(
                    ParameterId::from_bytes((index as u128 + 1).to_be_bytes()),
                    format!("parameter_{index}"),
                    u32::try_from(index).unwrap(),
                    integer_type,
                    None,
                )
            })
            .collect();
        let function = function(
            FunctionDomain::Server,
            parameters,
            rows_reference(TARGET),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        );
        let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let arguments = (0..parameter_count)
            .map(|index| {
                FunctionArgument::new(
                    ParameterId::from_bytes((index as u128 + 1).to_be_bytes()),
                    RuntimeValue::Integer(i32::try_from(index).unwrap()),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();

        validate_function_signature(&catalogue, &function).unwrap();
        let validated = validate_arguments(&catalogue, &function, &arguments).unwrap();

        assert_eq!(validated.len(), parameter_count);
    }

    #[test]
    fn lowering_uses_exact_stable_ids_typed_binds_and_an_unbound_typed_null() {
        let function = valid_function();
        let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let arguments = validate_arguments(&catalogue, &function, &valid_arguments()).unwrap();
        let lowered = lower_insert(&valid_plan(), &arguments).unwrap();

        assert_eq!(
            lowered.sql,
            "INSERT INTO _orna_data.t_10101010101010101010101010101010 (_orna_object_id, f_41414141414141414141414141414141, f_42424242424242424242424242424242, f_43434343434343434343434343434343, f_44444444444444444444444444444444) VALUES ($1, $2, $3, CAST(NULL AS int4), $4) RETURNING _orna_object_id AS c0",
        );
        assert_eq!(
            lowered.bind_types,
            vec![Type::BYTEA, Type::TEXT, Type::BOOL, Type::BYTEA],
        );
        assert_eq!(
            lowered.binds,
            vec![
                BindValue::Text(String::from("title")),
                BindValue::Boolean(true),
                BindValue::Bytes(OBJECT.to_bytes().to_vec()),
            ],
        );
        assert_eq!(lowered.sql.matches('$').count(), 4);
        for forbidden in [
            "semantic_target",
            "semantic_title",
            "semantic_insert",
            "semantic_created",
            "semantic_owner_parameter",
        ] {
            assert!(!lowered.sql.contains(forbidden));
        }
        assert!(!lowered.sql.contains("f_45454545454545454545454545454545"));
        assert!(lowered.sql.len() < SQL_LIMIT);
    }

    #[test]
    fn update_lowering_uses_stable_ids_typed_binds_and_exact_selector() {
        let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let function = valid_update_function();
        let plan = valid_update_plan();
        validate_plan_for_operation(
            &catalogue,
            &function,
            TARGET,
            &plan,
            MutationExecutionKind::Update,
        )
        .unwrap();
        let raw_arguments = valid_update_arguments();
        let arguments = validate_arguments(&catalogue, &function, &raw_arguments).unwrap();

        assert_eq!(
            selector_object(&plan, &raw_arguments).unwrap(),
            SELECTED_OBJECT,
        );
        let lowered = lower_update(&plan, &arguments).unwrap();
        assert_eq!(
            lowered.sql,
            "UPDATE _orna_data.t_10101010101010101010101010101010 SET f_41414141414141414141414141414141 = $1, f_44444444444444444444444444444444 = $2 WHERE _orna_object_id = $3 RETURNING _orna_object_id AS c0",
        );
        assert_eq!(
            lowered.bind_types,
            vec![Type::TEXT, Type::BYTEA, Type::BYTEA],
        );
        assert_eq!(
            lowered.binds,
            vec![
                BindValue::Text(String::from("title")),
                BindValue::Bytes(OBJECT.to_bytes().to_vec()),
                BindValue::Bytes(SELECTED_OBJECT.to_bytes().to_vec()),
            ],
        );
        for forbidden in [
            "semantic_target",
            "semantic_title",
            "semantic_insert",
            "semantic_updated",
            "semantic_selector_parameter",
        ] {
            assert!(!lowered.sql.contains(forbidden));
        }
    }

    #[test]
    fn delete_lowering_uses_only_stable_ids_and_the_exact_bytea_selector() {
        let catalogue = catalogue(target_fields(OTHER), true, Vec::new());
        let function = valid_delete_function();
        let plan = valid_delete_plan();
        validate_delete_plan(&catalogue, &function, &plan).unwrap();
        let raw_arguments = valid_delete_arguments();
        let arguments = validate_arguments(&catalogue, &function, &raw_arguments).unwrap();

        let lowered = lower_delete(&plan, &arguments).unwrap();

        assert_eq!(
            lowered.sql,
            "DELETE FROM _orna_data.t_10101010101010101010101010101010 WHERE _orna_object_id = $1 RETURNING _orna_object_id AS c0",
        );
        assert_eq!(lowered.bind_types, vec![Type::BYTEA]);
        assert_eq!(
            lowered.binds,
            vec![BindValue::Bytes(SELECTED_OBJECT.to_bytes().to_vec())],
        );
        for forbidden in [
            "semantic_target",
            "semantic_insert",
            "semantic_deleted",
            "semantic_selector_parameter",
        ] {
            assert!(!lowered.sql.contains(forbidden));
        }
        assert!(lowered.sql.len() < SQL_LIMIT);
    }

    #[test]
    fn lowering_reuses_one_owned_bind_for_repeated_parameter_assignments() {
        let text_type = ResolvedType::scalar(StandardScalar::CharacterLargeObject);
        let function = function(
            FunctionDomain::Server,
            vec![ParameterDefinition::new(
                PARAMETER_TITLE,
                "semantic_title_parameter",
                0,
                text_type,
                None,
            )],
            rows_reference(TARGET),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::Atomic),
            FunctionVolatility::Volatile,
        );
        let catalogue = catalogue(
            vec![
                field(FIELD_TITLE, "first", 0, text_type, false),
                field(FIELD_ENABLED, "second", 1, text_type, false),
                field(FIELD_COUNT, "third", 2, text_type, false),
            ],
            false,
            Vec::new(),
        );
        let parameter =
            || MutationExpression::parameter(FUNCTION, PARAMETER_TITLE, text_type).unwrap();
        let plan = ServerMutationPlan::new_insert(
            TARGET,
            [
                FieldAssignment::new(TARGET, FIELD_TITLE, parameter()),
                FieldAssignment::new(TARGET, FIELD_ENABLED, parameter()),
                FieldAssignment::new(TARGET, FIELD_COUNT, parameter()),
            ],
            TARGET,
        )
        .unwrap();
        validate_plan(&catalogue, &function, TARGET, &plan).unwrap();
        let arguments = validate_arguments(
            &catalogue,
            &function,
            &[FunctionArgument::new(
                PARAMETER_TITLE,
                RuntimeValue::Text(String::from("one owned payload")),
            )
            .unwrap()],
        )
        .unwrap();

        let lowered = lower_insert(&plan, &arguments).unwrap();

        assert_eq!(
            lowered.sql,
            "INSERT INTO _orna_data.t_10101010101010101010101010101010 (_orna_object_id, f_41414141414141414141414141414141, f_42424242424242424242424242424242, f_43434343434343434343434343434343) VALUES ($1, $2, $2, $2) RETURNING _orna_object_id AS c0",
        );
        assert_eq!(lowered.bind_types, vec![Type::BYTEA, Type::TEXT]);
        assert_eq!(
            lowered.binds,
            vec![BindValue::Text(String::from("one owned payload"))],
        );
    }

    #[test]
    fn bind_ownership_covers_every_runtime_storage_type() {
        let values = [
            (RuntimeValue::Boolean(true), BindValue::Boolean(true)),
            (RuntimeValue::Integer(-1), BindValue::Integer(-1)),
            (RuntimeValue::BigInt(2), BindValue::BigInt(2)),
            (
                RuntimeValue::Float(RuntimeFloat::new(3.5).unwrap()),
                BindValue::Float(3.5),
            ),
            (
                RuntimeValue::Text(String::from("text")),
                BindValue::Text(String::from("text")),
            ),
            (
                RuntimeValue::Bytes(vec![1, 2]),
                BindValue::Bytes(vec![1, 2]),
            ),
            (
                RuntimeValue::Reference {
                    target: OTHER,
                    object: OBJECT,
                },
                BindValue::Bytes(OBJECT.to_bytes().to_vec()),
            ),
        ];
        for (value, expected) in values {
            assert_eq!(
                BindValue::from_runtime(&value, PARAMETER_TITLE).unwrap(),
                expected,
            );
        }
    }

    #[test]
    fn public_errors_preserve_context_source_and_commit_state() {
        let context = ServerInsertContext::new(
            RevisionPair::new(
                SourceRevisionId::from_bytes([0x76; 16]),
                CatalogueRevisionId::from_bytes([0x77; 16]),
            ),
            FUNCTION,
            REVISION,
        );
        let not_committed = ServerInsertError::NotCommitted {
            context,
            source: Box::new(ServerInsertError::Argument {
                parameter: Some(PARAMETER_TITLE),
                rule: "the argument type does not match the declared parameter type",
            }),
        };
        assert_eq!(
            not_committed.commit_state(),
            ServerInsertCommitState::NotCommitted,
        );
        assert!(not_committed.source().is_some());
        assert_eq!(
            not_committed.to_string(),
            "the row was not added: a supplied function argument is invalid: the argument type does not match the declared parameter type",
        );

        let rejected = ServerInsertError::CommitRejected {
            context,
            target: TARGET,
            candidate: OBJECT,
            source: "port=invalid"
                .parse::<tokio_postgres::Config>()
                .unwrap_err(),
        };
        assert_eq!(
            rejected.to_string(),
            format!(
                "the database rejected the final save for object {}; no row was added",
                OBJECT.canonical(),
            ),
        );

        let unknown = ServerInsertError::CommitOutcomeUnknown {
            context,
            target: TARGET,
            candidate: OBJECT,
            source: "port=invalid"
                .parse::<tokio_postgres::Config>()
                .unwrap_err(),
        };
        assert_eq!(unknown.commit_state(), ServerInsertCommitState::Unknown);
        assert!(unknown.source().is_some());
        assert_eq!(
            unknown.to_string(),
            format!(
                "the connection failed while saving object {}; it is not known whether the row was added; do not retry automatically",
                OBJECT.canonical(),
            ),
        );

        let result = ServerInsertResult::new(
            context,
            TARGET,
            OBJECT,
            ResultColumn::new("created", ResolvedType::reference(TARGET), false).unwrap(),
        )
        .unwrap();
        let committed = ServerInsertError::CommittedButShutdownFailed {
            result: Box::new(result.clone()),
            source: Box::new(PostgresKernelError::CatalogueInvariant("shutdown test")),
        };
        assert_eq!(committed.commit_state(), ServerInsertCommitState::Committed);
        let ServerInsertError::CommittedButShutdownFailed {
            result: retained, ..
        } = committed
        else {
            unreachable!();
        };
        assert_eq!(*retained, result);
        assert_eq!(
            ServerInsertError::CommittedButShutdownFailed {
                result: Box::new(result),
                source: Box::new(PostgresKernelError::CatalogueInvariant("shutdown test")),
            }
            .to_string(),
            format!(
                "object {} was added, but the database connection did not close cleanly",
                OBJECT.canonical(),
            ),
        );
    }

    #[test]
    fn unique_reference_conflict_preserves_typed_context_and_not_committed_outcomes() {
        let context = ServerInsertContext::new(
            RevisionPair::new(
                SourceRevisionId::from_bytes([0x76; 16]),
                CatalogueRevisionId::from_bytes([0x77; 16]),
            ),
            FUNCTION,
            REVISION,
        );
        let conflict = || ServerMutationError::UniqueReferenceConflict {
            owner: TARGET,
            field: FIELD_OWNER,
            referenced_type: OTHER,
            source: "port=invalid"
                .parse::<tokio_postgres::Config>()
                .unwrap_err(),
        };

        let error = conflict();
        let ServerMutationError::UniqueReferenceConflict {
            owner,
            field,
            referenced_type,
            ..
        } = &error
        else {
            unreachable!();
        };
        assert_eq!(
            (*owner, *field, *referenced_type),
            (TARGET, FIELD_OWNER, OTHER)
        );
        assert_eq!(
            error.to_string(),
            "this reference is already used by another object"
        );
        assert_eq!(
            error.commit_state(),
            ServerMutationCommitState::NotCommitted
        );
        assert!(error.source().is_some());

        let insert = expect_insert_error(not_committed(context, server_error(conflict())));
        let ServerMutationError::NotCommitted {
            context: insert_context,
            source: insert_source,
        } = insert
        else {
            panic!("expected contextual INSERT conflict");
        };
        assert_eq!(insert_context, context);
        assert!(matches!(
            insert_source.as_ref(),
            ServerMutationError::UniqueReferenceConflict {
                owner: TARGET,
                field: FIELD_OWNER,
                referenced_type: OTHER,
                ..
            }
        ));

        let update = expect_update_error(update_not_committed(context, server_error(conflict())));
        let ServerUpdateError::NotCommitted {
            context: update_context,
            source: update_source,
        } = update
        else {
            panic!("expected contextual UPDATE conflict");
        };
        assert_eq!(update_context, context);
        assert!(matches!(
            update_source.as_ref(),
            ServerMutationError::UniqueReferenceConflict {
                owner: TARGET,
                field: FIELD_OWNER,
                referenced_type: OTHER,
                ..
            }
        ));
    }

    #[test]
    fn unique_reference_classifier_requires_exact_active_constraint_evidence() {
        let expected = UniqueReferenceConstraint {
            owner: TARGET,
            field: FIELD_OWNER,
            referenced_type: OTHER,
        };
        let constraints = UniqueReferenceConstraints {
            fields: vec![expected],
        };
        let expected_name = unique_constraint_name(FIELD_OWNER);

        assert_eq!(
            unique_reference_constraint(
                &constraints,
                Some(&SqlState::UNIQUE_VIOLATION),
                Some(&expected_name),
            ),
            Some(expected)
        );
        assert_eq!(
            unique_reference_constraint(&constraints, Some(&SqlState::UNIQUE_VIOLATION), None),
            None
        );
        assert_eq!(
            unique_reference_constraint(
                &constraints,
                Some(&SqlState::UNIQUE_VIOLATION),
                Some(&unique_constraint_name(FIELD_TITLE)),
            ),
            None
        );
        assert_eq!(
            unique_reference_constraint(
                &constraints,
                Some(&SqlState::UNIQUE_VIOLATION),
                Some("unrelated_unique_constraint"),
            ),
            None
        );
        assert_eq!(
            unique_reference_constraint(
                &constraints,
                Some(&SqlState::FOREIGN_KEY_VIOLATION),
                Some(&expected_name),
            ),
            None
        );
    }

    #[test]
    fn update_errors_preserve_match_context_and_retry_state() {
        let context = ServerUpdateContext::new(
            RevisionPair::new(
                SourceRevisionId::from_bytes([0x76; 16]),
                CatalogueRevisionId::from_bytes([0x77; 16]),
            ),
            FUNCTION,
            REVISION,
        );
        let not_committed = ServerUpdateError::NotCommitted {
            context,
            source: Box::new(ServerMutationError::Argument {
                parameter: Some(PARAMETER_SELECTOR),
                rule: "the argument type does not match the declared parameter type",
            }),
        };
        assert_eq!(
            not_committed.commit_state(),
            ServerUpdateCommitState::NotCommitted,
        );
        assert_eq!(
            not_committed.to_string(),
            "the object was not updated: a supplied function argument is invalid: the argument type does not match the declared parameter type",
        );

        let unknown = ServerUpdateError::CommitOutcomeUnknown {
            context,
            target: TARGET,
            selector: SELECTED_OBJECT,
            matched: true,
            source: "port=invalid"
                .parse::<tokio_postgres::Config>()
                .unwrap_err(),
        };
        assert_eq!(unknown.commit_state(), ServerUpdateCommitState::Unknown);
        assert_eq!(
            unknown.to_string(),
            format!(
                "the connection failed while saving object {}; it is not known whether the update committed; do not retry automatically",
                SELECTED_OBJECT.canonical(),
            ),
        );
        let ServerUpdateError::CommitOutcomeUnknown {
            target,
            selector,
            matched,
            ..
        } = unknown
        else {
            unreachable!();
        };
        assert_eq!(target, TARGET);
        assert_eq!(selector, SELECTED_OBJECT);
        assert!(matched);

        let result = ServerUpdateResult::new(
            context,
            TARGET,
            SELECTED_OBJECT,
            false,
            ResultColumn::new("updated", ResolvedType::reference(TARGET), false).unwrap(),
        )
        .unwrap();
        let committed = ServerUpdateError::CommittedButShutdownFailed {
            result: Box::new(result.clone()),
            source: Box::new(PostgresKernelError::CatalogueInvariant("shutdown test")),
        };
        assert_eq!(committed.commit_state(), ServerUpdateCommitState::Committed,);
        let ServerUpdateError::CommittedButShutdownFailed {
            result: retained, ..
        } = committed
        else {
            unreachable!();
        };
        assert_eq!(*retained, result);

        let wrapped = expect_update_error(update_not_committed(context, plan_invariant("test")));
        let ServerUpdateError::NotCommitted { source, .. } = wrapped else {
            panic!("expected a known-not-committed UPDATE failure");
        };
        assert!(matches!(
            *source,
            ServerMutationError::PlanInvariant { rule: "test" },
        ));
    }

    #[test]
    fn delete_errors_preserve_selector_match_result_and_retry_state() {
        let context = ServerDeleteContext::new(
            RevisionPair::new(
                SourceRevisionId::from_bytes([0x76; 16]),
                CatalogueRevisionId::from_bytes([0x77; 16]),
            ),
            FUNCTION,
            REVISION,
        );
        let not_committed = ServerDeleteError::NotCommitted {
            context,
            source: Box::new(ServerMutationError::Argument {
                parameter: Some(PARAMETER_SELECTOR),
                rule: "the argument type does not match the declared parameter type",
            }),
        };
        assert_eq!(
            not_committed.commit_state(),
            ServerDeleteCommitState::NotCommitted,
        );
        assert_eq!(
            not_committed.to_string(),
            "the object was not deleted: a supplied function argument is invalid: the argument type does not match the declared parameter type",
        );

        let unknown = ServerDeleteError::CommitOutcomeUnknown {
            context,
            target: TARGET,
            selector: SELECTED_OBJECT,
            matched: true,
            source: "port=invalid"
                .parse::<tokio_postgres::Config>()
                .unwrap_err(),
        };
        assert_eq!(unknown.commit_state(), ServerDeleteCommitState::Unknown);
        assert_eq!(
            unknown.to_string(),
            format!(
                "the connection failed while deleting object {}; it is not known whether the delete committed; do not retry automatically",
                SELECTED_OBJECT.canonical(),
            ),
        );
        let ServerDeleteError::CommitOutcomeUnknown {
            target,
            selector,
            matched,
            ..
        } = unknown
        else {
            unreachable!();
        };
        assert_eq!(target, TARGET);
        assert_eq!(selector, SELECTED_OBJECT);
        assert!(matched);

        let result = ServerDeleteResult::new(
            context,
            TARGET,
            SELECTED_OBJECT,
            true,
            ResultColumn::new(
                "deleted",
                ResolvedType::scalar(StandardScalar::Boolean),
                false,
            )
            .unwrap(),
        )
        .unwrap();
        let committed = ServerDeleteError::CommittedButShutdownFailed {
            result: Box::new(result.clone()),
            source: Box::new(PostgresKernelError::CatalogueInvariant("shutdown test")),
        };
        assert_eq!(committed.commit_state(), ServerDeleteCommitState::Committed);
        let ServerDeleteError::CommittedButShutdownFailed {
            result: retained, ..
        } = committed
        else {
            unreachable!();
        };
        assert_eq!(*retained, result);

        let wrapped = expect_delete_error(delete_not_committed(context, plan_invariant("test")));
        let ServerDeleteError::NotCommitted { source, .. } = wrapped else {
            panic!("expected a known-not-committed DELETE failure");
        };
        assert!(matches!(
            *source,
            ServerMutationError::PlanInvariant { rule: "test" },
        ));
    }

    #[test]
    fn delete_commit_classification_hides_constraint_timing() {
        assert_eq!(
            delete_commit_failure(Some(&SqlState::FOREIGN_KEY_VIOLATION)),
            DeleteCommitFailure::Restricted,
        );
        assert_eq!(
            delete_commit_failure(Some(&SqlState::RESTRICT_VIOLATION)),
            DeleteCommitFailure::Restricted,
        );
        assert_eq!(
            delete_commit_failure(Some(&SqlState::UNIQUE_VIOLATION)),
            DeleteCommitFailure::Rejected,
        );
        assert_eq!(delete_commit_failure(None), DeleteCommitFailure::Unknown,);
    }

    #[test]
    fn saved_function_errors_hide_internal_rules_and_give_one_recovery_action() {
        assert_eq!(
            ServerInsertError::Artifact {
                function: FUNCTION,
                rule: "internal artifact detail",
            }
            .to_string(),
            "the saved function is unsupported; redeploy it or contact the database administrator",
        );
        assert_eq!(
            ServerInsertError::PlanDecode(ServerMutationPlan::decode(&[]).unwrap_err()).to_string(),
            "the saved function cannot be read; redeploy it or contact the database administrator",
        );
        assert_eq!(
            ServerInsertError::PlanInvariant {
                rule: "internal invariant detail",
            }
            .to_string(),
            "the saved function is inconsistent with the active database; redeploy it or contact the database administrator",
        );
        assert_eq!(
            ServerInsertError::ReferenceEvidence {
                function: FUNCTION,
                rule: "internal evidence detail",
            }
            .to_string(),
            "the saved function is inconsistent with the active database; redeploy it or contact the database administrator",
        );
        assert_eq!(
            ServerInsertError::PreparedResult {
                rule: "one BYTEA column named c0",
            }
            .to_string(),
            "the database prepared an unexpected result; redeploy the function or contact the database administrator",
        );
        assert_eq!(
            ServerInsertError::ValueInvariant {
                rule: "identity must contain 16 bytes",
            }
            .to_string(),
            "the database returned an unexpected object identity; contact the database administrator",
        );
        assert_eq!(
            ServerInsertError::ComplexityLimit {
                category: "total size of text and binary arguments",
                maximum: VARIABLE_ARGUMENT_PAYLOAD_LIMIT,
            }
            .to_string(),
            format!(
                "the request is too large: total size of text and binary arguments limit is {VARIABLE_ARGUMENT_PAYLOAD_LIMIT}"
            ),
        );
        assert_eq!(
            ServerInsertError::ComplexityLimit {
                category: "saved function complexity",
                maximum: SQL_LIMIT,
            }
            .to_string(),
            format!("the request is too large: saved function complexity limit is {SQL_LIMIT}"),
        );
    }

    #[test]
    fn outer_kernel_error_keeps_the_public_server_insert_source() {
        let error = PostgresKernelError::ServerInsert(ServerInsertError::FunctionNotActive {
            pair: RevisionPair::new(
                SourceRevisionId::from_bytes([0x78; 16]),
                CatalogueRevisionId::from_bytes([0x79; 16]),
            ),
            function: FUNCTION,
        });
        assert!(error.source().is_some());
        assert_eq!(
            error.to_string(),
            "row creation failed: the requested function is not active; no row was added",
        );
    }
}
