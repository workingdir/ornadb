use super::*;

/// Immutable active state pinned for one SERVER SELECT execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServerSelectContext {
    pair: RevisionPair,
    function: FunctionId,
    function_revision: FunctionRevisionId,
}

impl ServerSelectContext {
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

/// The stable result of one validated SERVER `SELECT` execution.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerSelectResult {
    pair: RevisionPair,
    function: FunctionId,
    revision: FunctionRevisionId,
    rows: ResultRows,
}

impl ServerSelectResult {
    pub(crate) fn new(
        pair: RevisionPair,
        function: FunctionId,
        revision: FunctionRevisionId,
        rows: ResultRows,
    ) -> Self {
        Self {
            pair,
            function,
            revision,
            rows,
        }
    }

    /// Returns the source and catalogue pair read by this execution.
    pub const fn pair(&self) -> RevisionPair {
        self.pair
    }

    /// Returns the executed function identity.
    pub const fn function(&self) -> FunctionId {
        self.function
    }

    /// Returns the immutable function revision that supplied the plan.
    pub const fn function_revision(&self) -> FunctionRevisionId {
        self.revision
    }

    /// Returns validated rows in declared result-column order.
    pub fn rows(&self) -> &ResultRows {
        &self.rows
    }

    /// Transfers validated rows without cloning their value payloads.
    pub(crate) fn into_rows(self) -> ResultRows {
        self.rows
    }
}

/// A typed rejection of the initial SERVER `SELECT` execution subset.
#[non_exhaustive]
#[derive(Debug)]
pub enum ServerSelectError {
    /// Authorisation evidence does not cover the recovered active revision.
    AuthorisationMismatch {
        /// The immutable target covered by the authorisation evidence.
        authorised: Box<InvocationTarget>,
        /// The recovered active revision pair.
        active: RevisionPair,
    },
    /// The requested function is not in the recovered active catalogue.
    FunctionNotActive {
        /// The recovered active revision pair.
        pair: RevisionPair,
        /// The requested stable function identity.
        function: FunctionId,
    },
    /// A failure after an active function revision was pinned.
    Execution {
        /// The immutable active execution context.
        context: ServerSelectContext,
        /// The underlying typed failure.
        source: Box<ServerSelectError>,
    },
    /// PostgreSQL failed after an active function revision was pinned.
    Database {
        /// The immutable active execution context.
        context: ServerSelectContext,
        /// The PostgreSQL failure.
        source: tokio_postgres::Error,
    },
    /// A kernel validation failure after an active function revision was pinned.
    Kernel {
        /// The kernel failure that carries its native source chain.
        source: Box<PostgresKernelError>,
    },
    /// The function does not execute in the server domain.
    FunctionDomain { function: FunctionId },
    /// The function signature is outside the supported SERVER SELECT forms.
    FunctionSignature {
        function: FunctionId,
        rule: &'static str,
    },
    /// The function or its result is outside the raw-call SERVER boundary.
    RawTarget {
        /// The requested function identity.
        function: FunctionId,
        /// The exact rejected raw-call rule.
        rule: &'static str,
    },
    /// The active function has no exact active immutable revision record.
    CurrentRevision {
        function: FunctionId,
        revision: FunctionRevisionId,
    },
    /// The current revision does not contain the supported server-plan artifact.
    Artifact {
        function: FunctionId,
        rule: &'static str,
    },
    /// The canonical server plan cannot decode.
    PlanDecode(orna_artifact::server_plan::ServerPlanError),
    /// The pinned standard parameter-echo artifact cannot decode.
    ParameterEchoDecode(ServerParameterEchoError),
    /// The pinned standard json-encode artifact cannot decode.
    JsonEncodeDecode(JsonEncodePlanError),
    /// The pinned standard terminal-table artifact cannot decode.
    TerminalTableDecode(TerminalTablePlanError),
    /// The pinned standard csv-encode artifact cannot decode.
    CsvEncodeDecode(CsvEncodePlanError),
    /// A standard presenter cannot convert the bound value without loss.
    Presenter {
        /// The exact rejected presenter rule.
        rule: &'static str,
    },
    /// A standard presenter built an invalid opaque value payload.
    PresenterOpaque(OpaqueValueError),
    /// The plan does not agree with the recovered active catalogue.
    PlanInvariant { rule: &'static str },
    /// A saved `SELECT DISTINCT` function is outside the accepted runtime form.
    Distinct {
        /// The exact human-facing rule that failed.
        rule: &'static str,
    },
    /// The ordered durable definition references do not prove this plan.
    ReferenceEvidence {
        function: FunctionId,
        rule: &'static str,
    },
    /// Supplied runtime arguments do not equal the active signature.
    Argument {
        /// The related parameter identity, when one is available.
        parameter: Option<ParameterId>,
        /// The exact rejected rule.
        rule: &'static str,
    },
    /// A version-2 identity selection returned more than its one permitted row.
    Cardinality { rule: &'static str },
    /// The plan result cannot enter the initial runtime value subset.
    ResultRows(ResultRowsError),
    /// Returned values did not reconstruct the already validated result shape.
    ReturnedRows(ResultRowsError),
    /// PostgreSQL did not prepare the exact generated result shape.
    PreparedResult { rule: &'static str },
    /// PostgreSQL returned a value that does not match the generated result shape.
    RowDecode {
        /// The zero-based result row index.
        row: usize,
        /// The zero-based result column index.
        column: usize,
        /// The PostgreSQL conversion failure.
        source: tokio_postgres::Error,
    },
    /// A decoded PostgreSQL value violates an Orna runtime value invariant.
    ValueInvariant {
        /// The zero-based result row index.
        row: usize,
        /// The zero-based result column index.
        column: usize,
        /// The exact runtime rule that failed.
        rule: &'static str,
    },
    /// Canonical record bytes did not decode under the pinned active revision.
    ValueCodec {
        /// The zero-based result row index.
        row: usize,
        /// The zero-based result column index.
        column: usize,
        /// The canonical value-codec failure.
        source: ValueCodecError,
    },
    /// A variable result value exceeded its per-cell payload bound.
    VariablePayload {
        /// The zero-based result row index.
        row: usize,
        /// The zero-based result column index.
        column: usize,
        /// The largest permitted value payload.
        maximum: usize,
    },
    /// The plan exceeded one fixed execution complexity bound.
    ComplexityLimit {
        /// The bounded plan category.
        category: &'static str,
        /// The largest accepted value.
        maximum: usize,
    },
    /// The result exceeds the fixed row bound.
    RowLimit { maximum: usize },
    /// The result exceeds the fixed cell bound.
    CellLimit { maximum: usize },
    /// The result exceeds the fixed logical payload bound.
    PayloadLimit { maximum: usize },
}

impl fmt::Display for ServerSelectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthorisationMismatch { authorised, active } => write!(
                formatter,
                "authorised SERVER target {:?} does not match active pair {:?}",
                authorised, active
            ),
            Self::FunctionNotActive { pair, function } => {
                write!(
                    formatter,
                    "function {} is not active in pair {:?}",
                    function.canonical(),
                    pair
                )
            }
            Self::Execution { context, source } => {
                write!(
                    formatter,
                    "SERVER SELECT {} revision {} in pair {:?} failed: {source}",
                    context.function().canonical(),
                    context.function_revision().canonical(),
                    context.pair()
                )
            }
            Self::Database { context, source } => {
                write!(
                    formatter,
                    "SERVER SELECT {} revision {} in pair {:?} had a PostgreSQL failure: {source}",
                    context.function().canonical(),
                    context.function_revision().canonical(),
                    context.pair()
                )
            }
            Self::Kernel { source } => write!(
                formatter,
                "active SERVER SELECT kernel validation failed: {source}"
            ),
            Self::FunctionDomain { function } => {
                write!(
                    formatter,
                    "function {} is not a SERVER function",
                    function.canonical()
                )
            }
            Self::FunctionSignature { function, rule } => {
                write!(
                    formatter,
                    "function {} has an unsupported signature: {rule}",
                    function.canonical()
                )
            }
            Self::RawTarget { function, rule } => write!(
                formatter,
                "function {} is not an available raw SERVER target: {rule}",
                function.canonical()
            ),
            Self::CurrentRevision { function, revision } => write!(
                formatter,
                "function {} has no active revision {}",
                function.canonical(),
                revision.canonical(),
            ),
            Self::Artifact { function, rule } => {
                write!(
                    formatter,
                    "function {} has an unsupported artifact: {rule}",
                    function.canonical()
                )
            }
            Self::PlanDecode(error) => write!(formatter, "cannot decode server plan: {error}"),
            Self::ParameterEchoDecode(error) => {
                write!(
                    formatter,
                    "cannot decode server parameter-echo artifact: {error}"
                )
            }
            Self::JsonEncodeDecode(error) => {
                write!(
                    formatter,
                    "cannot decode server json-encode artifact: {error}"
                )
            }
            Self::TerminalTableDecode(error) => {
                write!(
                    formatter,
                    "cannot decode server terminal-table artifact: {error}"
                )
            }
            Self::CsvEncodeDecode(error) => {
                write!(
                    formatter,
                    "cannot decode server csv-encode artifact: {error}"
                )
            }
            Self::Presenter { rule } => {
                write!(formatter, "standard presenter execution failed: {rule}")
            }
            Self::PresenterOpaque(error) => {
                write!(
                    formatter,
                    "standard presenter built an invalid opaque value: {error}"
                )
            }
            Self::PlanInvariant { rule } => {
                write!(formatter, "server plan invariant failed: {rule}")
            }
            Self::Distinct { rule } => {
                write!(
                    formatter,
                    "saved SELECT DISTINCT function cannot run: {rule}"
                )
            }
            Self::ReferenceEvidence { function, rule } => write!(
                formatter,
                "function {} has invalid definition-reference evidence: {rule}",
                function.canonical(),
            ),
            Self::Argument { rule, .. } => {
                write!(formatter, "a supplied function argument is invalid: {rule}")
            }
            Self::Cardinality { rule } => {
                write!(formatter, "SERVER SELECT returned too many rows: {rule}")
            }
            Self::ResultRows(error) => write!(formatter, "invalid server result shape: {error}"),
            Self::ReturnedRows(error) => {
                write!(formatter, "returned server rows are invalid: {error}")
            }
            Self::PreparedResult { rule } => {
                write!(formatter, "prepared server result is invalid: {rule}")
            }
            Self::RowDecode {
                row,
                column,
                source,
            } => {
                write!(
                    formatter,
                    "cannot decode result row {row} column {column}: {source}"
                )
            }
            Self::ValueInvariant { row, column, rule } => {
                write!(
                    formatter,
                    "result row {row} column {column} violates {rule}"
                )
            }
            Self::ValueCodec {
                row,
                column,
                source,
            } => write!(
                formatter,
                "cannot decode canonical result row {row} column {column}: {source}",
            ),
            Self::VariablePayload {
                row,
                column,
                maximum,
            } => write!(
                formatter,
                "result row {row} column {column} exceeds {maximum} payload bytes"
            ),
            Self::ComplexityLimit { category, maximum } => {
                write!(formatter, "server plan {category} exceeds {maximum}")
            }
            Self::RowLimit { maximum } => write!(formatter, "server result exceeds {maximum} rows"),
            Self::CellLimit { maximum } => {
                write!(formatter, "server result exceeds {maximum} cells")
            }
            Self::PayloadLimit { maximum } => {
                write!(
                    formatter,
                    "server result exceeds {maximum} logical payload bytes"
                )
            }
        }
    }
}

impl Error for ServerSelectError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Execution { source, .. } => Some(source),
            Self::Database { source, .. } => Some(source),
            Self::Kernel { source } => Some(source),
            Self::PlanDecode(error) => Some(error),
            Self::ParameterEchoDecode(error) => Some(error),
            Self::JsonEncodeDecode(error) => Some(error),
            Self::TerminalTableDecode(error) => Some(error),
            Self::CsvEncodeDecode(error) => Some(error),
            Self::PresenterOpaque(error) => Some(error),
            Self::ResultRows(error) => Some(error),
            Self::ReturnedRows(error) => Some(error),
            Self::RowDecode { source, .. } => Some(source),
            Self::ValueCodec { source, .. } => Some(source),
            Self::FunctionNotActive { .. }
            | Self::AuthorisationMismatch { .. }
            | Self::FunctionDomain { .. }
            | Self::FunctionSignature { .. }
            | Self::RawTarget { .. }
            | Self::CurrentRevision { .. }
            | Self::Artifact { .. }
            | Self::PlanInvariant { .. }
            | Self::Distinct { .. }
            | Self::Presenter { .. }
            | Self::ReferenceEvidence { .. }
            | Self::Argument { .. }
            | Self::Cardinality { .. }
            | Self::PreparedResult { .. }
            | Self::ValueInvariant { .. }
            | Self::VariablePayload { .. }
            | Self::ComplexityLimit { .. }
            | Self::RowLimit { .. }
            | Self::CellLimit { .. }
            | Self::PayloadLimit { .. } => None,
        }
    }
}
