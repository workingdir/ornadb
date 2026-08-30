use super::*;

/// Immutable active state pinned for one SERVER mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServerMutationContext {
    pair: RevisionPair,
    function: FunctionId,
    function_revision: FunctionRevisionId,
}

impl ServerMutationContext {
    pub(super) const fn new(
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
    pub(super) fn new(
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
    pub(super) fn new(
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
    pub(super) fn new(
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
    /// A checked record constructor could not be built from the active catalogue.
    RecordValue(RecordValueError),
    /// A checked record constructor could not be encoded as canonical bytes.
    ValueCodec(ValueCodecError),
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
    /// An exact Text value is already assigned to another object.
    UniqueTextConflict {
        /// The object type that owns the unique Text field.
        owner: TypeId,
        /// The exact unique Text field.
        field: FieldId,
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
            | Self::RecordValue(_)
            | Self::ValueCodec(_)
            | Self::UniqueReferenceConflict { .. }
            | Self::UniqueTextConflict { .. }
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
            Self::RecordValue(_) => formatter.write_str(
                "the saved record constructor is inconsistent with the active database",
            ),
            Self::ValueCodec(_) => formatter.write_str(
                "the record constructor cannot be encoded as an active canonical value",
            ),
            Self::UniqueReferenceConflict { .. } => {
                formatter.write_str("this reference is already used by another object")
            }
            Self::UniqueTextConflict { .. } => {
                formatter.write_str("this text value is already used by another object")
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
            | Self::UniqueTextConflict { source, .. }
            | Self::CommitRejected { source, .. }
            | Self::CommitOutcomeUnknown { source, .. } => Some(source),
            Self::PlanDecode(error) => Some(error),
            Self::ResultRows(error) => Some(error),
            Self::RecordValue(error) => Some(error),
            Self::ValueCodec(error) => Some(error),
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
