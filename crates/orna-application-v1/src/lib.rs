//! The authoritative application boundary for Orna 1.0.
//!
//! This crate deliberately keeps orchestration small: syntax parsing and the
//! semantic resolver/type checker remain in their owning crates, while this
//! boundary is the one place an application source becomes an admitted
//! evaluator program. Runtime table work is staged against the exact captured
//! activation context and is never published by this crate.

use orna_evaluator_v1::{
    AdmittedReplSession, EffectHandler, Environment, EvaluationError, Functions, Limits,
    PureFunction, RelationPage, StepBudget, invoke_named, invoke_named_with_effects,
    reference_standard_profile, reference_standard_sources,
};
use orna_foundation_v1::{CanonicalSnapshot, CanonicalValue, OvbRaw, SafeText};
use orna_live_v1::{
    Error as LiveError, LiveAdminEffectDispatcher, LiveApplication, LiveApplicationWorkLease,
    LiveEvalResponse, LiveEvalTransaction,
};
use orna_protocol_v1::{Envelope, Message, PresentNode, ResultStatus};
use orna_runtime_v1::{
    NoFault, RuntimeActivationContext, RuntimeError, RuntimePublicationMetadataRows,
    RuntimeTableActivationSnapshot, RuntimeTableRows, StagedTableActivation, TableMutation,
};
use orna_semantic_v1::{
    Catalogue, ModuleInput, Namespace, SymbolKind, TableSchema, analyze_with_catalogue,
};
use orna_syntax_v1::{
    CaseArm, Declaration, Expr, Statement, StringSegment, parse_module_with_file,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::UNIX_EPOCH,
};

const DIGEST_DOMAIN: &[u8] = b"ORNA-ACTIVATION-DIGEST\0";
const SOURCE_MUTATION_DOMAIN: &[u8] = b"ORNA-SOURCE-MUTATION\0";
const MAX_ADMITTED_REPL_SESSIONS: usize = 4096;

/// Errors raised before an application is allowed to execute.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApplicationError {
    /// The frozen syntax parser rejected the source.
    Parse(Vec<String>),
    /// Name resolution or static type/effect checking rejected the source.
    Semantic(Vec<String>),
    /// The requested entry function was not present in the admitted module.
    MissingEntry(String),
    /// The bounded evaluator rejected the already-admitted program.
    Evaluation(String),
    /// Runtime staging rejected a malformed mutation or activation.
    Runtime(String),
    /// Canonical snapshot encoding failed while computing an activation digest.
    DigestEncoding,
    /// A table effect targeted a relation without admitted write schema.
    UnadmittedTable(String),
    /// The evaluator rejected an admitted effect outside the table boundary.
    EffectRejected(String),
    /// The source requested an effect that could not be authorized or run.
    SourceEffectFailed(String),
    /// Async runtime effects are supported only as the activation's terminal
    /// result, so their real result is available before the activation commits.
    UnsupportedSourceEffectPlacement,
}

impl fmt::Display for ApplicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(_) => formatter.write_str("application source failed syntax admission"),
            Self::Semantic(_) => {
                formatter.write_str("application source failed semantic admission")
            }
            Self::MissingEntry(name) => {
                write!(formatter, "application entry `{name}` was not admitted")
            }
            Self::Evaluation(code) => write!(formatter, "application evaluation failed: {code}"),
            Self::Runtime(message) => {
                write!(formatter, "runtime activation staging failed: {message}")
            }
            Self::DigestEncoding => {
                formatter.write_str("activation context could not be canonically encoded")
            }
            Self::UnadmittedTable(name) => {
                write!(formatter, "table `{name}` has no admitted write schema")
            }
            Self::EffectRejected(code) => {
                write!(formatter, "admitted effect was rejected: {code}")
            }
            Self::SourceEffectFailed(code) => write!(formatter, "source effect failed: {code}"),
            Self::UnsupportedSourceEffectPlacement => formatter.write_str(
                "runtime-backed source effects must be the activation's terminal result",
            ),
        }
    }
}

impl std::error::Error for ApplicationError {}

/// A source-backed application authority. The catalogue and evaluator limits
/// are captured once so every admission in this authority uses the same
/// resolver and bounded execution policy.
#[derive(Clone, Debug)]
pub struct ApplicationAuthority {
    catalogue: Catalogue,
    limits: Limits,
}

impl ApplicationAuthority {
    /// Creates an authority with an explicit declaration catalogue and limits.
    #[must_use]
    pub fn new(catalogue: Catalogue, limits: Limits) -> Self {
        Self { catalogue, limits }
    }

    /// Admits one source module through parser, resolver and type/effect checks.
    /// No evaluator function is exposed until all three stages succeed.
    pub fn admit_module(
        &self,
        logical_path: impl Into<String>,
        source: impl Into<String>,
        entry: impl Into<String>,
    ) -> Result<AdmittedApplication, ApplicationError> {
        let logical_path = logical_path.into();
        let source = source.into();
        let entry = entry.into();
        self.limits
            .check_source(&source)
            .map_err(|error| ApplicationError::Evaluation(error.code().to_owned()))?;

        let parsed = parse_module_with_file(&source, logical_path.clone());
        if !parsed.is_ok() {
            return Err(ApplicationError::Parse(
                parsed
                    .diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.code.to_owned())
                    .collect(),
            ));
        }

        let analysis = analyze_with_catalogue(
            &[ModuleInput::new(logical_path.clone(), source.clone())],
            &self.catalogue,
        );
        if !analysis.is_ok() {
            return Err(ApplicationError::Semantic(
                analysis
                    .diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.code().to_owned())
                    .collect(),
            ));
        }

        let mut functions = Functions::new();
        for item in &parsed.value.items {
            if let Declaration::Function { signature, body } = &item.declaration {
                functions.insert(
                    signature.name.clone(),
                    PureFunction {
                        parameters: signature.parameters.clone(),
                        body: body.clone(),
                        environment: Environment::new(),
                    },
                );
            }
        }
        if !functions.contains_key(&entry) {
            return Err(ApplicationError::MissingEntry(entry));
        }
        let admitted_namespace = module_namespace(&logical_path);

        Ok(AdmittedApplication {
            logical_path,
            source_digest: Sha256::digest(source.as_bytes()).into(),
            requires_publication_metadata: source.contains("sys.Storage")
                || source.contains("sys.MaintenanceJob"),
            entry,
            functions,
            limits: self.limits,
            module_header: analysis
                .modules
                .get(&admitted_namespace)
                .cloned()
                .expect("successful analysis records the admitted module header"),
        })
    }

    /// Executes the admitted entry through the production bounded evaluator.
    pub fn evaluate(
        &self,
        application: &AdmittedApplication,
        arguments: &Environment,
    ) -> Result<CanonicalValue, ApplicationError> {
        invoke_named(
            &application.entry,
            &application.functions,
            arguments,
            application.limits,
        )
        .map_err(|error: EvaluationError| ApplicationError::Evaluation(error.code().to_owned()))
    }

    /// Executes the admitted entry while staging every admitted table effect
    /// the source performs.
    ///
    /// Table writes traverse the evaluator's effect boundary into
    /// [`SourceMutationEffectHandler`], are validated against the admitted
    /// catalogue schema, and are returned for the runtime's owner-fenced
    /// commit. A pure source yields an empty mutation batch.
    pub fn evaluate_staged(
        &self,
        application: &AdmittedApplication,
        arguments: &Environment,
    ) -> Result<StagedActivation, ApplicationError> {
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler = SourceMutationEffectHandler::new(tables);
        let value = invoke_named_with_effects(
            &application.entry,
            &application.functions,
            arguments,
            application.limits,
            &mut handler,
        )
        .map_err(|error: EvaluationError| {
            ApplicationError::EffectRejected(error.code().to_owned())
        })?;
        let mutations = handler.into_mutations()?;
        Ok(StagedActivation {
            value,
            mutations,
            snapshot_generation: None,
        })
    }

    /// Evaluates table mutations against one activation-pinned table snapshot.
    /// This is required for operations whose result depends on the previous
    /// row value (`update`, patching `upsert`, and `rekey`). The returned work
    /// can only be staged against the same captured generation.
    pub fn evaluate_staged_with_table_snapshot(
        &self,
        application: &AdmittedApplication,
        arguments: &Environment,
        snapshot: &RuntimeTableActivationSnapshot,
    ) -> Result<StagedActivation, ApplicationError> {
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler = SourceMutationEffectHandler::with_table_rows(
            tables,
            snapshot.table_rows().clone(),
        )?;
        let value = invoke_named_with_effects(
            &application.entry,
            &application.functions,
            arguments,
            application.limits,
            &mut handler,
        )
        .map_err(|error: EvaluationError| {
            ApplicationError::EffectRejected(error.code().to_owned())
        })?;
        let mutations = handler.into_mutations()?;
        Ok(StagedActivation {
            value,
            mutations,
            snapshot_generation: Some(snapshot.context().capture().generation_digest()),
        })
    }

    /// Evaluates admitted source with a preloaded durable publication snapshot
    /// available to the evaluator's `sys.Storage` and `sys.MaintenanceJob`
    /// relation scans. Snapshot loading remains asynchronous at the caller;
    /// evaluation and relation paging stay bounded and synchronous here.
    pub fn evaluate_staged_with_publication_rows(
        &self,
        application: &AdmittedApplication,
        arguments: &Environment,
        publication_rows: RuntimePublicationMetadataRows,
    ) -> Result<StagedActivation, ApplicationError> {
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler = SourceMutationEffectHandler::with_publication_rows(
            tables,
            publication_rows,
        );
        let value = invoke_named_with_effects(
            &application.entry,
            &application.functions,
            arguments,
            application.limits,
            &mut handler,
        )
        .map_err(|error: EvaluationError| {
            ApplicationError::EffectRejected(error.code().to_owned())
        })?;
        let mutations = handler.into_mutations()?;
        Ok(StagedActivation {
            value,
            mutations,
            snapshot_generation: None,
        })
    }

    /// Evaluates admitted source while dispatching its terminal runtime effect
    /// through an explicit trusted host capability before returning staged
    /// writes to the runtime for commit.
    ///
    /// Ordinary [`Self::evaluate_staged`] and the live application adapter do
    /// not receive this capability and continue to reject administrative
    /// effects. The async effect must be the entry's final expression: this
    /// keeps the evaluator's source order intact and makes the actual host
    /// result available to the caller before any staged writes are committed.
    pub async fn evaluate_staged_with_async_effects(
        &self,
        application: &AdmittedApplication,
        arguments: &Environment,
        context: &RuntimeActivationContext,
        dispatcher: &dyn AsyncApplicationEffectDispatcher,
    ) -> Result<StagedActivation, ApplicationError> {
        validate_terminal_runtime_effect(application)?;
        let tables = admitted_table_schemas(&application.module_header);
        let publication_rows = dispatcher
            .publication_metadata_rows()
            .await
            .map_err(ApplicationError::SourceEffectFailed)?;
        let mut handler = AsyncSourceMutationEffectHandler::new(tables, publication_rows);
        let mut value = invoke_named_with_effects(
            &application.entry,
            &application.functions,
            arguments,
            application.limits,
            &mut handler,
        )
        .map_err(|error: EvaluationError| {
            ApplicationError::EffectRejected(error.code().to_owned())
        })?;
        let (mutations, effects) = handler.into_parts()?;
        if !mutations.is_empty()
            && let Some(effect) = effects.iter().find(|effect| {
                matches!(
                    effect,
                    ApplicationEffectRequest::PauseStream { .. }
                        | ApplicationEffectRequest::ResumeStream { .. }
                )
            })
        {
            // The admin transition uses its own durable transaction. A source
            // activation with staged table writes must finish first.
            let code = dispatcher
                .reject_staged_admin_effect(effect.clone(), context)
                .await
                .err()
                .unwrap_or_else(|| "sys.admin.busy".to_owned());
            return Err(ApplicationError::SourceEffectFailed(code));
        }
        for effect in effects {
            value = dispatcher
                .dispatch(effect, context)
                .await
                .map_err(ApplicationError::SourceEffectFailed)?;
        }
        Ok(StagedActivation {
            value,
            mutations,
            snapshot_generation: None,
        })
    }

    /// Computes the canonical digest for staged table work in one captured
    /// runtime activation. The context pin and ordered mutation bytes are both
    /// included, so a digest cannot be reused across CWD generations.
    pub fn canonical_digest(
        context: &RuntimeActivationContext,
        mutations: &[TableMutation],
    ) -> Result<[u8; 32], ApplicationError> {
        let snapshot_bytes = CanonicalValue::new(context.capture().snapshot().raw())
            .ok()
            .and_then(|value| value.encode().ok())
            .ok_or(ApplicationError::DigestEncoding)?;
        let mut digest = Sha256::new();
        digest.update(DIGEST_DOMAIN);
        digest.update(context.capture().generation_digest());
        put_bytes(&mut digest, &snapshot_bytes);
        let elapsed = context
            .activation_time()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        digest.update(elapsed.as_secs().to_be_bytes());
        digest.update(elapsed.subsec_nanos().to_be_bytes());
        for mutation in mutations {
            digest.update(mutation.id());
            put_bytes(&mut digest, mutation.table().as_bytes());
            put_bytes(&mut digest, mutation.key());
            match mutation.value() {
                Some(value) => {
                    digest.update([1]);
                    put_bytes(&mut digest, value);
                }
                None => digest.update([0]),
            }
        }
        Ok(digest.finalize().into())
    }

    /// Stages a typed mutation for a captured activation. Publication remains
    /// the runtime's owner-fenced commit operation.
    pub fn stage_mutation(
        &self,
        context: RuntimeActivationContext,
        mutation: TableMutation,
    ) -> Result<StagedTableActivation, ApplicationError> {
        self.stage_mutations(context, vec![mutation])
    }

    /// Stages an ordered mutation batch for one captured activation. An empty
    /// batch is rejected by the runtime boundary; pure activations never
    /// enter this path.
    pub fn stage_mutations(
        &self,
        context: RuntimeActivationContext,
        mutations: Vec<TableMutation>,
    ) -> Result<StagedTableActivation, ApplicationError> {
        let digest = Self::canonical_digest(&context, &mutations)?;
        StagedTableActivation::from_source(context, mutations, digest, Arc::new(NoFault))
            .map_err(|error: RuntimeError| ApplicationError::Runtime(error.to_string()))
    }
}

/// The evaluated result of one admitted source activation together with the
/// ordered table mutations it performed.
#[derive(Clone, Debug)]
pub struct StagedActivation {
    value: CanonicalValue,
    mutations: Vec<TableMutation>,
    snapshot_generation: Option<[u8; 32]>,
}

/// A runtime operation requested by admitted application source.
///
/// Portable runtime handles are data only. A trusted dispatcher must resolve
/// each handle under the current owner and pinned activation context before
/// invoking a runtime transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApplicationEffectRequest {
    PauseStream {
        stream: CanonicalValue,
        reason: Option<String>,
    },
    ResumeStream {
        stream: CanonicalValue,
    },
    CancelInvocation {
        invocation: CanonicalValue,
        reason: Option<String>,
    },
}

/// Async result returned by one explicitly authorized source-effect dispatch.
pub type ApplicationEffectFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CanonicalValue, String>> + 'a>>;

pub type ApplicationPublicationRowsFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<RuntimePublicationMetadataRows>, String>> + 'a>>;

/// Capability supplied only by a trusted local runtime coordinator.
///
/// Implementations must resolve portable references against the authenticated
/// writer and `context`; references themselves never grant authority.
pub trait AsyncApplicationEffectDispatcher {
    fn publication_metadata_rows(&self) -> ApplicationPublicationRowsFuture<'_> {
        Box::pin(async { Ok(None) })
    }

    fn dispatch<'a>(
        &'a self,
        effect: ApplicationEffectRequest,
        context: &'a RuntimeActivationContext,
    ) -> ApplicationEffectFuture<'a>;

    fn reject_staged_admin_effect<'a>(
        &'a self,
        _effect: ApplicationEffectRequest,
        _context: &'a RuntimeActivationContext,
    ) -> ApplicationEffectFuture<'a> {
        Box::pin(async { Err("sys.admin.busy".to_owned()) })
    }
}

struct LiveEffectAdapter<'a>(&'a dyn LiveAdminEffectDispatcher);

impl AsyncApplicationEffectDispatcher for LiveEffectAdapter<'_> {
    fn publication_metadata_rows(&self) -> ApplicationPublicationRowsFuture<'_> {
        Box::pin(async move { self.0.publication_metadata_rows().await })
    }

    fn dispatch<'a>(
        &'a self,
        effect: ApplicationEffectRequest,
        context: &'a RuntimeActivationContext,
    ) -> ApplicationEffectFuture<'a> {
        match effect {
            ApplicationEffectRequest::PauseStream { stream, reason } => {
                self.0.pause_stream(stream, reason, context)
            }
            ApplicationEffectRequest::ResumeStream { stream } => {
                self.0.resume_stream(stream, context)
            }
            ApplicationEffectRequest::CancelInvocation { .. } => {
                Box::pin(async { Err("sys.invoke.effect_unavailable".to_owned()) })
            }
        }
    }

    fn reject_staged_admin_effect<'a>(
        &'a self,
        effect: ApplicationEffectRequest,
        context: &'a RuntimeActivationContext,
    ) -> ApplicationEffectFuture<'a> {
        match effect {
            ApplicationEffectRequest::PauseStream { stream, reason } => {
                self.0.reject_staged_admin_effect(stream, reason, false, context)
            }
            ApplicationEffectRequest::ResumeStream { stream } => {
                self.0
                    .reject_staged_admin_effect(stream, None, true, context)
            }
            ApplicationEffectRequest::CancelInvocation { .. } => {
                Box::pin(async { Err("sys.admin.busy".to_owned()) })
            }
        }
    }
}

impl StagedActivation {
    #[must_use]
    pub fn value(&self) -> &CanonicalValue {
        &self.value
    }

    #[must_use]
    pub fn mutations(&self) -> &[TableMutation] {
        &self.mutations
    }

    /// Stages this activation's mutations against the captured context for
    /// the owner-fenced runtime commit. Empty mutation batches are rejected
    /// by the runtime boundary, so a pure activation must not call this.
    pub fn stage(
        &self,
        authority: &ApplicationAuthority,
        context: &RuntimeActivationContext,
    ) -> Result<StagedTableActivation, ApplicationError> {
        if self.snapshot_generation.is_some_and(|generation| {
            generation != context.capture().generation_digest()
        }) {
            return Err(ApplicationError::Runtime(
                "table rows and commit context refer to different CWD generations".into(),
            ));
        }
        // Mutation IDs are unique in the runtime's durable ledger. The source
        // handler's IDs identify an ordered write within a source batch, so
        // bind them to this captured activation before crossing that boundary.
        // Replaying the same staged activation against the same context keeps
        // the IDs stable, while a later activation can repeat the same write.
        let activation_digest = ApplicationAuthority::canonical_digest(context, &self.mutations)?;
        let mutations = self
            .mutations
            .iter()
            .enumerate()
            .map(|(ordinal, mutation)| {
                let mut digest = Sha256::new();
                digest.update(b"ORNA-SOURCE-ACTIVATION-MUTATION\0");
                digest.update(activation_digest);
                digest.update((ordinal as u64).to_be_bytes());
                digest.update(mutation.id());
                let id: [u8; 16] = digest.finalize()[..16]
                    .try_into()
                    .map_err(|_| ApplicationError::DigestEncoding)?;
                let staged = match (mutation.rekey_to(), mutation.is_insert()) {
                    (Some(new_key), _) => mutation
                        .value()
                        .ok_or_else(|| {
                            ApplicationError::Runtime("re-key had no replacement row".into())
                        })
                        .and_then(|value| {
                            TableMutation::rekey(
                                id,
                                mutation.table(),
                                mutation.key().to_vec(),
                                new_key.to_vec(),
                                value.to_vec(),
                            )
                            .map_err(|error| ApplicationError::Runtime(error.to_string()))
                        }),
                    (None, true) => mutation
                        .value()
                        .ok_or_else(|| {
                            ApplicationError::Runtime("insert had no replacement row".into())
                        })
                        .and_then(|value| {
                            TableMutation::insert(
                                id,
                                mutation.table(),
                                mutation.key().to_vec(),
                                value.to_vec(),
                            )
                            .map_err(|error| ApplicationError::Runtime(error.to_string()))
                        }),
                    (None, false) => TableMutation::new(
                        id,
                        mutation.table(),
                        mutation.key().to_vec(),
                        mutation.value().map(<[u8]>::to_vec),
                    )
                    .map_err(|error: RuntimeError| {
                        ApplicationError::Runtime(error.to_string())
                    }),
                };
                staged
            })
            .collect::<Result<Vec<_>, _>>()?;
        authority.stage_mutations(context.clone(), mutations)
    }
}

/// Lowers admitted source table effects into validated runtime mutations.
///
/// Every effect argument vector is evaluated exactly once by the evaluator
/// before reaching this handler. Only declared tables with admitted write
/// schema are accepted; any other write effect fails closed with a redacted
/// evaluation error instead of performing an unrecorded write.
#[derive(Debug)]
pub struct SourceMutationEffectHandler {
    tables: BTreeMap<String, TableSchema>,
    mutations: Vec<TableMutation>,
    next_ordinal: u64,
    publication_rows: Option<RuntimePublicationMetadataRows>,
    captured_rows: Option<BTreeMap<String, BTreeMap<Vec<u8>, CanonicalValue>>>,
    overlay: BTreeMap<String, BTreeMap<Vec<u8>, Option<CanonicalValue>>>,
}

impl SourceMutationEffectHandler {
    #[must_use]
    pub fn new(tables: BTreeMap<String, TableSchema>) -> Self {
        Self {
            tables,
            mutations: Vec::new(),
            next_ordinal: 0,
            publication_rows: None,
            captured_rows: None,
            overlay: BTreeMap::new(),
        }
    }

    fn with_table_rows(
        tables: BTreeMap<String, TableSchema>,
        rows: RuntimeTableRows,
    ) -> Result<Self, ApplicationError> {
        let mut handler = Self::new(tables);
        let mut captured = BTreeMap::new();
        for table in handler.tables.keys() {
            let table_rows = rows
                .get(table)
                .ok_or_else(|| ApplicationError::UnadmittedTable(table.clone()))?;
            let mut decoded = BTreeMap::new();
            for (key, value) in table_rows {
                let row = CanonicalValue::decode(value).map_err(|_| {
                    ApplicationError::Runtime("captured table row was not canonical".into())
                })?;
                handler
                    .row_matches_schema(&handler.tables[table], &row)
                    .map_err(|_| {
                        ApplicationError::Runtime("captured table row failed its schema".into())
                    })?;
                if handler
                    .key_from_row(&handler.tables[table], &row)
                    .map_err(|_| {
                        ApplicationError::Runtime("captured table row had an invalid key".into())
                    })?
                    != *key
                {
                    return Err(ApplicationError::Runtime(
                        "captured table row key did not match its row".into(),
                    ));
                }
                if decoded.insert(key.clone(), row).is_some() {
                    return Err(ApplicationError::Runtime(
                        "captured table snapshot had duplicate keys".into(),
                    ));
                }
            }
            captured.insert(table.clone(), decoded);
        }
        handler.captured_rows = Some(captured);
        Ok(handler)
    }

    fn with_publication_rows(
        tables: BTreeMap<String, TableSchema>,
        publication_rows: RuntimePublicationMetadataRows,
    ) -> Self {
        Self {
            tables,
            mutations: Vec::new(),
            next_ordinal: 0,
            publication_rows: Some(publication_rows),
            captured_rows: None,
            overlay: BTreeMap::new(),
        }
    }

    /// Consumes the handler after validating every recorded mutation.
    pub fn into_mutations(self) -> Result<Vec<TableMutation>, ApplicationError> {
        self.into_validated_mutations()
    }

    fn into_validated_mutations(self) -> Result<Vec<TableMutation>, ApplicationError> {
        for mutation in &self.mutations {
            TableMutation::new(
                mutation.id(),
                mutation.table(),
                mutation.key().to_vec(),
                mutation.value().map(<[u8]>::to_vec),
            )
            .map_err(|_| ApplicationError::Runtime("recorded mutation was invalid".into()))?;
        }
        Ok(self.mutations)
    }

    fn effect_error(code: &'static str) -> EvaluationError {
        EvaluationError::redacted(SafeText::new(code).expect("static safe code"))
    }

    fn record(
        &mut self,
        table: &str,
        key: Vec<u8>,
        value: Option<CanonicalValue>,
    ) -> Result<(), EvaluationError> {
        self.record_with_kind(table, key, value, None, false)
    }

    fn record_insert(
        &mut self,
        table: &str,
        key: Vec<u8>,
        value: CanonicalValue,
    ) -> Result<(), EvaluationError> {
        self.record_with_kind(table, key, Some(value), None, true)
    }

    fn record_rekey(
        &mut self,
        table: &str,
        old_key: Vec<u8>,
        new_key: Vec<u8>,
        value: CanonicalValue,
    ) -> Result<(), EvaluationError> {
        self.record_with_kind(table, old_key, Some(value), Some(new_key), false)
    }

    fn record_with_kind(
        &mut self,
        table: &str,
        key: Vec<u8>,
        value: Option<CanonicalValue>,
        rekey_to: Option<Vec<u8>>,
        insert_only: bool,
    ) -> Result<(), EvaluationError> {
        let encoded = value
            .as_ref()
            .map(CanonicalValue::encode)
            .transpose()
            .map_err(|_| Self::effect_error("ORNA-EVAL-TABLE-ROW"))?;
        let mut digest = Sha256::new();
        digest.update(SOURCE_MUTATION_DOMAIN);
        digest.update(table.as_bytes());
        digest.update([0]);
        digest.update(&key);
        digest.update([u8::from(insert_only), u8::from(rekey_to.is_some())]);
        if let Some(new_key) = &rekey_to {
            digest.update(new_key);
        }
        digest.update(self.next_ordinal.to_be_bytes());
        if let Some(bytes) = &encoded {
            digest.update([1]);
            digest.update(bytes);
        } else {
            digest.update([0]);
        }
        let id: [u8; 16] = digest.finalize()[..16]
            .try_into()
            .map_err(|_| Self::effect_error("ORNA-EVAL-TABLE-ROW"))?;
        let mutation = match (rekey_to, insert_only, encoded) {
            (Some(new_key), false, Some(value)) => {
                TableMutation::rekey(id, table, key, new_key, value)
            }
            (None, true, Some(value)) => TableMutation::insert(id, table, key, value),
            (None, false, value) => TableMutation::new(id, table, key, value),
            _ => return Err(Self::effect_error("ORNA-EVAL-TABLE-ROW")),
        }
        .map_err(|_| Self::effect_error("ORNA-EVAL-TABLE-ROW"))?;
        self.next_ordinal = self
            .next_ordinal
            .checked_add(1)
            .ok_or_else(|| Self::effect_error("ORNA-EVAL-LIMIT"))?;
        self.mutations.push(mutation);
        Ok(())
    }

    fn current_row(
        &self,
        table: &str,
        key: &[u8],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        if let Some(row) = self.overlay.get(table).and_then(|rows| rows.get(key)) {
            return Ok(row.clone());
        }
        let captured = self
            .captured_rows
            .as_ref()
            .ok_or_else(|| Self::effect_error("ORNA-EVAL-TABLE-SNAPSHOT"))?;
        Ok(captured.get(table).and_then(|rows| rows.get(key)).cloned())
    }

    fn current_row_if_known(
        &self,
        table: &str,
        key: &[u8],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        if let Some(row) = self.overlay.get(table).and_then(|rows| rows.get(key)) {
            return Ok(row.clone());
        }
        // Overlay entries shadow captured rows: a tombstone frees a key, while
        // a replacement row keeps that key occupied for later re-key attempts.
        Ok(self
            .captured_rows
            .as_ref()
            .and_then(|captured| captured.get(table))
            .and_then(|rows| rows.get(key))
            .cloned())
    }

    fn admitted_table_name(&self, path: &str) -> Option<&str> {
        if let Some((name, _)) = self.tables.get_key_value(path) {
            return Some(name);
        }
        let short_name = path.rsplit('.').next()?;
        let mut matches = self
            .tables
            .keys()
            .filter(|name| name.as_str() == short_name || name.ends_with(&format!(".{short_name}")));
        let found = matches.next()?;
        if matches.next().is_some() {
            None
        } else {
            Some(found)
        }
    }

    fn patch_row(
        &self,
        schema: &TableSchema,
        patch: &CanonicalValue,
        row: &CanonicalValue,
        permit_keys: bool,
    ) -> Result<CanonicalValue, EvaluationError> {
        let admission = self.admission(schema)?;
        let (OvbRaw::Map(existing), OvbRaw::Map(changes)) = (row.raw(), patch.raw()) else {
            return Err(Self::effect_error("ORNA-EVAL-TABLE-ROW"));
        };
        let mut fields = BTreeMap::<String, OvbRaw>::new();
        for (field, value) in existing {
            let OvbRaw::Text(field) = field else {
                return Err(Self::effect_error("ORNA-EVAL-TABLE-ROW"));
            };
            fields.insert(field.clone(), value.clone());
        }
        for (field, value) in changes {
            let OvbRaw::Text(field) = field else {
                return Err(Self::effect_error("ORNA-EVAL-TABLE-ROW"));
            };
            if !schema.fields.contains_key(field)
                || admission.computed.contains(field)
                || (!permit_keys && admission.keys.iter().any(|(key, _)| key == field))
            {
                return Err(Self::effect_error("ORNA-EVAL-TABLE-ROW"));
            }
            fields.insert(field.clone(), value.clone());
        }
        let mut entries = fields
            .into_iter()
            .map(|(name, value)| (OvbRaw::Text(name), value))
            .collect::<Vec<_>>();
        entries.sort_by(|(left, _), (right, _)| {
            canonical_map_key(left).cmp(&canonical_map_key(right))
        });
        let merged = CanonicalValue::new(OvbRaw::Map(entries))
            .map_err(|_| Self::effect_error("ORNA-EVAL-TABLE-ROW"))?;
        self.row_matches_schema(schema, &merged)?;
        Ok(merged)
    }

    fn key_components(
        &self,
        schema: &TableSchema,
        key: &CanonicalValue,
    ) -> Result<Vec<CanonicalValue>, EvaluationError> {
        let count = self.admission(schema)?.keys.len();
        if count == 1 {
            return Ok(vec![key.clone()]);
        }
        let OvbRaw::Array(parts) = key.raw() else {
            return Err(Self::effect_error("ORNA-EVAL-TABLE-KEY"));
        };
        if parts.len() != count {
            return Err(Self::effect_error("ORNA-EVAL-TABLE-KEY"));
        }
        parts
            .iter()
            .cloned()
            .map(|part| {
                CanonicalValue::new(part)
                    .map_err(|_| Self::effect_error("ORNA-EVAL-TABLE-KEY"))
            })
            .collect()
    }

    fn table(&self, name: &str) -> Result<&TableSchema, EvaluationError> {
        self.tables
            .get(name)
            .ok_or_else(|| Self::effect_error("ORNA-EVAL-TABLE-UNADMITTED"))
    }

    fn admission<'a>(
        &self,
        schema: &'a TableSchema,
    ) -> Result<&'a orna_semantic_v1::TableAdmission, EvaluationError> {
        schema
            .admission
            .as_ref()
            .ok_or_else(|| Self::effect_error("ORNA-EVAL-TABLE-UNADMITTED"))
    }

    fn key_from_row(
        &self,
        schema: &TableSchema,
        row: &CanonicalValue,
    ) -> Result<Vec<u8>, EvaluationError> {
        let OvbRaw::Map(entries) = row.raw() else {
            return Err(Self::effect_error("ORNA-EVAL-TABLE-ROW"));
        };
        let admission = self.admission(schema)?;
        let mut components = Vec::with_capacity(admission.keys.len());
        for (name, _) in &admission.keys {
            let value = entries
                .iter()
                .find_map(|(key, field)| match key {
                    OvbRaw::Text(field_name) if field_name == name => {
                        CanonicalValue::new(field.clone()).ok()
                    }
                    _ => None,
                })
                .ok_or_else(|| Self::effect_error("ORNA-EVAL-TABLE-KEY"))?;
            components.push(value);
        }
        encoded_table_key(&components)
    }

    fn row_matches_schema(
        &self,
        schema: &TableSchema,
        row: &CanonicalValue,
    ) -> Result<(), EvaluationError> {
        let OvbRaw::Map(entries) = row.raw() else {
            return Err(Self::effect_error("ORNA-EVAL-TABLE-ROW"));
        };
        let admission = self.admission(schema)?;
        for (key, _) in entries {
            let OvbRaw::Text(field) = key else {
                return Err(Self::effect_error("ORNA-EVAL-TABLE-ROW"));
            };
            if !schema.fields.contains_key(field.as_str())
                || admission.computed.contains(field.as_str())
            {
                return Err(Self::effect_error("ORNA-EVAL-TABLE-ROW"));
            }
        }
        for required in &admission.required {
            if !entries
                .iter()
                .any(|(key, _)| matches!(key, OvbRaw::Text(field) if field == required))
            {
                return Err(Self::effect_error("ORNA-EVAL-TABLE-ROW"));
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
struct AsyncSourceMutationEffectHandler {
    mutations: SourceMutationEffectHandler,
    effects: Vec<ApplicationEffectRequest>,
    publication_rows: Option<RuntimePublicationMetadataRows>,
}

impl AsyncSourceMutationEffectHandler {
    fn new(
        tables: BTreeMap<String, TableSchema>,
        publication_rows: Option<RuntimePublicationMetadataRows>,
    ) -> Self {
        Self {
            mutations: SourceMutationEffectHandler::new(tables),
            effects: Vec::new(),
            publication_rows,
        }
    }

    fn into_parts(
        self,
    ) -> Result<(Vec<TableMutation>, Vec<ApplicationEffectRequest>), ApplicationError> {
        Ok((self.mutations.into_validated_mutations()?, self.effects))
    }
}

impl EffectHandler for AsyncSourceMutationEffectHandler {
    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        if source_function_path(callee).as_deref() == Some("sys.admin.pause_stream") {
            let request = match arguments {
                [stream] => ApplicationEffectRequest::PauseStream {
                    stream: stream.clone(),
                    reason: None,
                },
                [stream, reason] => {
                    let reason = match reason.raw() {
                        OvbRaw::Null => None,
                        OvbRaw::Text(reason) => Some(reason.clone()),
                        _ => {
                            return Err(SourceMutationEffectHandler::effect_error(
                                "ORNA-EVAL-TYPE",
                            ));
                        }
                    };
                    ApplicationEffectRequest::PauseStream {
                        stream: stream.clone(),
                        reason,
                    }
                }
                _ => {
                    return Err(SourceMutationEffectHandler::effect_error(
                        "ORNA-EVAL-ARGUMENT",
                    ));
                }
            };
            self.effects.push(request);
            // The async host result replaces this placeholder before the
            // staged activation is returned to its owner for publication.
            return CanonicalValue::new(OvbRaw::Bool(false))
                .map(Some)
                .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-VALUE"));
        }
        if source_function_path(callee).as_deref() == Some("sys.admin.resume_stream") {
            let [stream] = arguments else {
                return Err(SourceMutationEffectHandler::effect_error(
                    "ORNA-EVAL-ARGUMENT",
                ));
            };
            self.effects.push(ApplicationEffectRequest::ResumeStream {
                stream: stream.clone(),
            });
            return CanonicalValue::new(OvbRaw::Bool(false))
                .map(Some)
                .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-VALUE"));
        }
        if source_function_path(callee).as_deref() == Some("sys.cancel") {
            let request = match arguments {
                [invocation] => ApplicationEffectRequest::CancelInvocation {
                    invocation: invocation.clone(),
                    reason: None,
                },
                [invocation, reason] => {
                    let reason = match reason.raw() {
                        OvbRaw::Null => None,
                        OvbRaw::Text(reason) => Some(reason.clone()),
                        _ => {
                            return Err(SourceMutationEffectHandler::effect_error(
                                "ORNA-EVAL-TYPE",
                            ));
                        }
                    };
                    ApplicationEffectRequest::CancelInvocation {
                        invocation: invocation.clone(),
                        reason,
                    }
                }
                _ => {
                    return Err(SourceMutationEffectHandler::effect_error(
                        "ORNA-EVAL-ARGUMENT",
                    ));
                }
            };
            self.effects.push(request);
            return CanonicalValue::new(OvbRaw::Bool(false))
                .map(Some)
                .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-VALUE"));
        }
        self.mutations.handle(callee, arguments)
    }

    fn handle_with_budget(
        &mut self,
        callee: &Expr,
        arguments: &[CanonicalValue],
        budget: &mut StepBudget,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        self.handle(callee, arguments).map(|result| {
            // The existing synchronous table handler does not debit
            // additional work; runtime dispatch remains bounded by its
            // own operation budget.
            let _ = budget;
            result
        })
    }

    fn scan_relation_page(
        &mut self,
        source: &str,
        after: Option<&[u8]>,
        limit: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<RelationPage>, EvaluationError> {
        if limit == 0 {
            return Ok(Some(RelationPage {
                rows: Vec::new(),
                next: None,
            }));
        }
        let Some(publication_rows) = self.publication_rows.as_ref() else {
            return Ok(None);
        };
        let row = match source {
            "sys.Storage" => &publication_rows.sys_storage,
            "sys.MaintenanceJob" => &publication_rows.maintenance_job,
            _ => return Ok(None),
        };
        if after.is_some() {
            return Ok(Some(RelationPage {
                rows: Vec::new(),
                next: None,
            }));
        }
        budget.debit(1)?;
        Ok(Some(RelationPage {
            rows: vec![row.clone()],
            next: None,
        }))
    }
}

fn source_function_path(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Name { text, .. } => Some(text.clone()),
        Expr::Field { base, name, .. } => Some(format!("{}.{}", source_function_path(base)?, name)),
        _ => None,
    }
}

fn validate_terminal_runtime_effect(
    application: &AdmittedApplication,
) -> Result<(), ApplicationError> {
    for (name, function) in &application.functions {
        let count = runtime_effect_call_count(&function.body);
        if name == &application.entry {
            if count > 0 && (count != 1 || !is_terminal_runtime_effect_call(&function.body)) {
                return Err(ApplicationError::UnsupportedSourceEffectPlacement);
            }
        } else if count > 0 {
            // A helper call could consume the placeholder before the async
            // dispatcher supplies the actual operation result.
            return Err(ApplicationError::UnsupportedSourceEffectPlacement);
        }
    }
    Ok(())
}

fn is_runtime_source_effect_path(path: &str) -> bool {
    matches!(
        path,
        "sys.admin.pause_stream" | "sys.admin.resume_stream" | "sys.cancel"
    )
}

fn is_terminal_runtime_effect_call(expression: &Expr) -> bool {
    match expression {
        Expr::Group { inner, .. } => is_terminal_runtime_effect_call(inner),
        Expr::Call { callee, .. } => source_function_path(callee)
            .as_deref()
            .is_some_and(is_runtime_source_effect_path),
        Expr::Block {
            statements, tail, ..
        } => {
            !statements
                .iter()
                .any(|statement| runtime_effect_statement_count(statement) != 0)
                && tail.as_deref().is_some_and(is_terminal_runtime_effect_call)
        }
        _ => false,
    }
}

fn runtime_effect_call_count(expression: &Expr) -> usize {
    let own = usize::from(matches!(
        expression,
        Expr::Call { callee, .. }
            if source_function_path(callee).as_deref().is_some_and(is_runtime_source_effect_path)
    ));
    own + match expression {
        Expr::Name { .. } | Expr::Literal { .. } | Expr::ReplBinding { .. } => 0,
        Expr::InterpolatedString { segments, .. } => segments
            .iter()
            .map(|segment| match segment {
                StringSegment::Text { .. } => 0,
                StringSegment::Expression { value, .. } => runtime_effect_call_count(value),
            })
            .sum(),
        Expr::Unary { rhs, .. } | Expr::Group { inner: rhs, .. } => runtime_effect_call_count(rhs),
        Expr::Binary { lhs, rhs, .. } => {
            runtime_effect_call_count(lhs) + runtime_effect_call_count(rhs)
        }
        Expr::Range { lower, upper, .. } => lower
            .iter()
            .chain(upper.iter())
            .map(|bound| runtime_effect_call_count(bound))
            .sum(),
        Expr::Call {
            callee, arguments, ..
        }
        | Expr::GenericCall {
            callee, arguments, ..
        } => {
            runtime_effect_call_count(callee)
                + arguments
                    .iter()
                    .map(|argument| runtime_effect_call_count(&argument.value))
                    .sum::<usize>()
        }
        Expr::Index { base, index, .. } => {
            runtime_effect_call_count(base) + runtime_effect_call_count(index)
        }
        Expr::Field { base, .. } => runtime_effect_call_count(base),
        Expr::Tuple { elements, .. } | Expr::List { elements, .. } => {
            elements.iter().map(runtime_effect_call_count).sum()
        }
        Expr::Record { fields, .. } | Expr::Nominal { fields, .. } => fields
            .iter()
            .map(|field| runtime_effect_call_count(&field.value))
            .sum(),
        Expr::Lambda { body, .. } => runtime_effect_call_count(body),
        Expr::Block {
            statements, tail, ..
        } => {
            statements
                .iter()
                .map(runtime_effect_statement_count)
                .sum::<usize>()
                + tail
                    .as_deref()
                    .map(runtime_effect_call_count)
                    .unwrap_or_default()
        }
        Expr::Control {
            condition,
            body,
            arms,
            alternate,
            ..
        } => {
            condition
                .as_deref()
                .map(runtime_effect_call_count)
                .unwrap_or_default()
                + body
                    .as_deref()
                    .map(runtime_effect_call_count)
                    .unwrap_or_default()
                + arms.iter().map(runtime_effect_arm_count).sum::<usize>()
                + alternate
                    .as_deref()
                    .map(runtime_effect_call_count)
                    .unwrap_or_default()
        }
    }
}

fn runtime_effect_arm_count(arm: &CaseArm) -> usize {
    arm.guard
        .as_ref()
        .map(runtime_effect_call_count)
        .unwrap_or_default()
        + runtime_effect_call_count(&arm.body)
}

fn runtime_effect_statement_count(statement: &Statement) -> usize {
    match statement {
        Statement::Let { value, .. }
        | Statement::Assert { value, .. }
        | Statement::Expression { value, .. }
        | Statement::Control { value, .. }
        | Statement::Assignment { value, .. } => runtime_effect_call_count(value),
        Statement::Return { value, .. } | Statement::Break { value, .. } => value
            .as_ref()
            .map(runtime_effect_call_count)
            .unwrap_or_default(),
        Statement::Continue { .. } => 0,
    }
}

impl EffectHandler for SourceMutationEffectHandler {
    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        let Expr::Field { base, name, .. } = callee else {
            return Ok(None);
        };
        if !matches!(name.as_str(), "insert" | "upsert" | "update" | "delete" | "rekey") {
            return Ok(None);
        }
        let Some(path) = expression_name_path(base) else {
            return Ok(None);
        };
        let Some(table) = self.admitted_table_name(&path).map(str::to_owned) else {
            return Ok(None);
        };
        let schema = self.table(&table)?;
        match name.as_str() {
            "insert" => {
                let [row] = arguments else {
                    return Err(Self::effect_error("ORNA-EVAL-TABLE-ARGUMENT"));
                };
                self.row_matches_schema(schema, row)?;
                let key = self.key_from_row(schema, row)?;
                if self.current_row_if_known(&table, &key)?.is_some() {
                    return Err(Self::effect_error("ORNA-EVAL-TABLE-DUPLICATE-KEY"));
                }
                self.record_insert(&table, key.clone(), row.clone())?;
                self.overlay
                    .entry(table.to_owned())
                    .or_default()
                    .insert(key, Some(row.clone()));
                Ok(Some(row.clone()))
            }
            "upsert" => {
                let [patch] = arguments else {
                    return Err(Self::effect_error("ORNA-EVAL-TABLE-ARGUMENT"));
                };
                let key = self.key_from_row(schema, patch)?;
                let row = if let Some(existing) = self.current_row(&table, &key)? {
                    self.patch_row(schema, patch, &existing, true)?
                } else {
                    self.row_matches_schema(schema, patch)?;
                    patch.clone()
                };
                let key = self.key_from_row(schema, &row)?;
                self.record(&table, key.clone(), Some(row.clone()))?;
                self.overlay
                    .entry(table.to_owned())
                    .or_default()
                    .insert(key, Some(row.clone()));
                Ok(Some(row))
            }
            "update" => {
                let [key, patch] = arguments else {
                    return Err(Self::effect_error("ORNA-EVAL-TABLE-ARGUMENT"));
                };
                let key_bytes = encoded_key(key)?;
                let existing = self
                    .current_row(&table, &key_bytes)?
                    .ok_or_else(|| Self::effect_error("ORNA-EVAL-TABLE-MISSING-ROW"))?;
                let row = self.patch_row(schema, patch, &existing, false)?;
                if self.key_from_row(schema, &row)? != key_bytes {
                    return Err(Self::effect_error("ORNA-EVAL-TABLE-KEY"));
                }
                self.record(&table, key_bytes.clone(), Some(row.clone()))?;
                self.overlay
                    .entry(table.to_owned())
                    .or_default()
                    .insert(key_bytes, Some(row.clone()));
                Ok(Some(row))
            }
            "delete" => {
                let [key] = arguments else {
                    return Err(Self::effect_error("ORNA-EVAL-TABLE-ARGUMENT"));
                };
                let admission = self.admission(schema)?;
                if admission.keys.is_empty() {
                    return Err(Self::effect_error("ORNA-EVAL-TABLE-UNADMITTED"));
                }
                let key = encoded_key(key)?;
                if self.current_row_if_known(&table, &key)?.is_none()
                    && self.captured_rows.is_some()
                {
                    return Err(Self::effect_error("ORNA-EVAL-TABLE-MISSING-ROW"));
                }
                self.record(&table, key, None)?;
                self.overlay
                    .entry(table.to_owned())
                    .or_default()
                    .insert(encoded_key(&arguments[0])?, None);
                Ok(Some(CanonicalValue::unit()))
            }
            "rekey" => {
                let [old_key, new_key] = arguments else {
                    return Err(Self::effect_error("ORNA-EVAL-TABLE-ARGUMENT"));
                };
                let admission = self.admission(schema)?;
                if admission.automatic_key || admission.keys.is_empty() {
                    return Err(Self::effect_error("ORNA-EVAL-TABLE-UNADMITTED"));
                }
                let old_key_bytes = encoded_key(old_key)?;
                let new_key_bytes = encoded_key(new_key)?;
                if old_key_bytes == new_key_bytes {
                    return Err(Self::effect_error("ORNA-EVAL-TABLE-DUPLICATE-KEY"));
                }
                // Resolve the source through the current activation overlay
                // before recording intent: a stale key after an earlier
                // successful re-key is a rejected effect, not a new identity.
                // If recovery reuses that source key, including with an
                // insert, later calls follow its new occupant and its updates.
                // Re-key resolves the latest full overlay row, retaining
                // patches made to an inserted identity before this call.
                let existing = self
                    .current_row(&table, &old_key_bytes)?
                    .ok_or_else(|| Self::effect_error("ORNA-EVAL-TABLE-MISSING-ROW"))?;
                // Preflight the destination before recording or changing the
                // overlay so a caught re-key failure leaves later tail work
                // with both original row identities intact.
                // Because preflight reads the overlay, a recovery that moved
                // the destination occupant makes that key available to retry.
                // If recovery then inserts a replacement there, that new row
                // becomes the occupant that a retry must preserve.
                // Every retry observes the latest overlay after recovery work.
                // A previously relocated row can reclaim the key and block it again.
                if self.current_row_if_known(&table, &new_key_bytes)?.is_some() {
                    return Err(Self::effect_error("ORNA-EVAL-TABLE-DUPLICATE-KEY"));
                }
                let parts = self.key_components(schema, new_key)?;
                let mut key_entries = parts
                    .iter()
                    .zip(&admission.keys)
                    .map(|(part, (name, _))| {
                        (OvbRaw::Text(name.clone()), part.raw().clone())
                    })
                    .collect::<Vec<_>>();
                key_entries.sort_by(|(left, _), (right, _)| {
                    canonical_map_key(left).cmp(&canonical_map_key(right))
                });
                let key_patch = CanonicalValue::new(OvbRaw::Map(key_entries))
                    .map_err(|_| Self::effect_error("ORNA-EVAL-TABLE-KEY"))?;
                let row = self.patch_row(schema, &key_patch, &existing, true)?;
                if self.key_from_row(schema, &row)? != new_key_bytes {
                    return Err(Self::effect_error("ORNA-EVAL-TABLE-KEY"));
                }
                self.record_rekey(
                    &table,
                    old_key_bytes.clone(),
                    new_key_bytes.clone(),
                    row.clone(),
                )?;
                let rows = self.overlay.entry(table.to_owned()).or_default();
                rows.insert(old_key_bytes, None);
                rows.insert(new_key_bytes, Some(row.clone()));
                Ok(Some(row))
            }
            // Read-shaped table calls stay on the ordinary evaluator path;
            // this boundary owns write lowering only.
            _ => Ok(None),
        }
    }

    fn handle_with_budget(
        &mut self,
        callee: &Expr,
        arguments: &[CanonicalValue],
        _budget: &mut StepBudget,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        self.handle(callee, arguments)
    }

    fn scan_relation_page(
        &mut self,
        source: &str,
        after: Option<&[u8]>,
        limit: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<RelationPage>, EvaluationError> {
        if limit == 0 {
            return Ok(Some(RelationPage {
                rows: Vec::new(),
                next: None,
            }));
        }
        let Some(publication_rows) = self.publication_rows.as_ref() else {
            return Ok(None);
        };
        let row = match source {
            "sys.Storage" => &publication_rows.sys_storage,
            "sys.MaintenanceJob" => &publication_rows.maintenance_job,
            _ => return Ok(None),
        };
        if after.is_some() {
            return Ok(Some(RelationPage {
                rows: Vec::new(),
                next: None,
            }));
        }
        budget.debit(1)?;
        Ok(Some(RelationPage {
            rows: vec![row.clone()],
            next: None,
        }))
    }
}

fn encoded_key(key: &CanonicalValue) -> Result<Vec<u8>, EvaluationError> {
    key.encode()
        .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-KEY"))
}

fn expression_name_path(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Name { text, .. } => Some(text.clone()),
        Expr::Field { base, name, .. } => {
            Some(format!("{}.{}", expression_name_path(base)?, name))
        }
        _ => None,
    }
}

fn canonical_map_key(key: &OvbRaw) -> Vec<u8> {
    CanonicalValue::new(key.clone())
        .and_then(|value| value.encode())
        .expect("record field names are canonical OVB text values")
}

fn encoded_table_key(components: &[CanonicalValue]) -> Result<Vec<u8>, EvaluationError> {
    match components {
        [single] => encoded_key(single),
        _ => CanonicalValue::new(OvbRaw::Array(
            components.iter().map(|value| value.raw().clone()).collect(),
        ))
        .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-KEY"))
        .and_then(|key| encoded_key(&key)),
    }
}

fn admitted_table_schemas(
    header: &orna_semantic_v1::ModuleHeader,
) -> BTreeMap<String, TableSchema> {
    header
        .symbols
        .iter()
        .filter(|(_, symbol)| symbol.kind == SymbolKind::Table)
        .filter_map(|(name, symbol)| {
            symbol
                .table_schema
                .clone()
                .filter(|schema| schema.admission.is_some())
                .map(|schema| (name.clone(), schema))
        })
        .collect()
}

fn module_namespace(logical_path: &str) -> Namespace {
    // Analysis also returns catalogue and standard-library headers; select
    // the original admitted module instead of whichever namespace sorts first.
    let mut parts = logical_path.split('/').map(str::to_owned).collect::<Vec<_>>();
    if let Some(file) = parts.pop() {
        let stem = file.strip_suffix(".orna").unwrap_or(&file);
        if stem != "main" {
            parts.push(stem.to_owned());
        }
    }
    Namespace(parts)
}

impl From<ApplicationError> for LiveError {
    fn from(error: ApplicationError) -> Self {
        match error {
            ApplicationError::SourceEffectFailed(code) if code == "sys.admin.busy" => {
                Self::AdminBusy
            }
            _ => Self::ApplicationRejected,
        }
    }
}

/// Mutable evaluator and admission state isolated to one remote REPL session.
#[derive(Clone, Debug)]
struct ApplicationReplSession {
    repl: AdmittedReplSession,
}

/// Source-backed application adapter for the live protocol.
///
/// The host remains responsible for canonical envelope and fingerprint
/// validation and owns every durable commit. With a captured activation
/// context the adapter returns a staged [`LiveEvalTransaction`]; without one,
/// admitted source effects fail closed instead of silently executing writes.
#[derive(Clone, Debug)]
pub struct ApplicationLiveAdapter {
    authority: ApplicationAuthority,
    logical_path: String,
    entry: String,
    sessions: Arc<Mutex<BTreeMap<[u8; 16], ApplicationReplSession>>>,
    watches: Arc<Mutex<BTreeMap<([u8; 16], [u8; 16]), ApplicationWatch>>>,
    runtime_identity: Option<([u8; 16], [u8; 16])>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ApplicationWatch {
    source: String,
    snapshot: CanonicalSnapshot,
    present: PresentNode,
    revision: u64,
}

impl ApplicationLiveAdapter {
    /// Creates an adapter for the deterministic remote-evaluation module.
    #[must_use]
    pub fn new(authority: ApplicationAuthority) -> Self {
        Self {
            authority,
            logical_path: "remote_eval.orna".to_owned(),
            entry: "main".to_owned(),
            sessions: Arc::new(Mutex::new(BTreeMap::new())),
            watches: Arc::new(Mutex::new(BTreeMap::new())),
            runtime_identity: None,
        }
    }

    /// Pins live snapshots to the clone runtime selected by the executable
    /// host. A watch cannot invent or switch its database/runtime identity.
    #[must_use]
    pub fn with_runtime_identity(mut self, database: [u8; 16], runtime: [u8; 16]) -> Self {
        self.runtime_identity = Some((database, runtime));
        self
    }

    /// Sets the logical module path and entry used for future Eval messages.
    #[must_use]
    pub fn with_module(
        mut self,
        logical_path: impl Into<String>,
        entry: impl Into<String>,
    ) -> Self {
        self.logical_path = logical_path.into();
        self.entry = entry.into();
        self
    }

    /// Admits the Eval message's source through the authority.
    fn evaluate_eval_message(&self, message: &Message) -> Result<AdmittedApplication, LiveError> {
        let Message::Eval { source, .. } = message else {
            return Err(LiveError::ApplicationRejected);
        };
        self.authority
            .admit_module(
                self.logical_path.clone(),
                source.clone(),
                self.entry.clone(),
            )
            .map_err(LiveError::from)
    }

    /// Keeps the existing complete-module entry point for clients that send
    /// an admitted application module. Ordinary Eval input uses the per-session
    /// ephemeral REPL below.
    fn is_module_entry(&self, message: &Message) -> bool {
        let Message::Eval { source, .. } = message else {
            return false;
        };
        let parsed = parse_module_with_file(source, self.logical_path.clone());
        parsed.is_ok()
            && parsed.value.items.iter().any(|item| {
                matches!(
                    &item.declaration,
                    Declaration::Function { signature, .. } if signature.name == self.entry
                )
            })
    }

    fn new_repl_session(&self) -> Result<AdmittedReplSession, LiveError> {
        let sources = reference_standard_sources().into_iter().collect::<Vec<_>>();
        let catalogue = self
            .authority
            .catalogue
            .clone()
            .with_standard_sources(&reference_standard_profile(), sources.clone())
            .map_err(|_| LiveError::ApplicationRejected)?;
        AdmittedReplSession::from_catalogue(&[], catalogue, sources, self.authority.limits)
            .map_err(|_| LiveError::ApplicationRejected)
    }

    fn repl_candidate(&self, session: [u8; 16]) -> Result<AdmittedReplSession, LiveError> {
        {
            let sessions = self
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(existing) = sessions.get(&session) {
                return Ok(existing.repl.clone());
            }
            if sessions.len() >= MAX_ADMITTED_REPL_SESSIONS {
                return Err(LiveError::ApplicationRejected);
            }
        }
        self.new_repl_session()
    }

    fn publish_repl_candidate(&self, session: [u8; 16], repl: AdmittedReplSession) {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session, ApplicationReplSession { repl });
    }

    fn repl_table_schemas(&self) -> BTreeMap<String, TableSchema> {
        analyze_with_catalogue(&[], &self.authority.catalogue)
            .modules
            .values()
            .flat_map(admitted_table_schemas)
            .collect()
    }

    /// Builds the success envelope for one evaluated request.
    fn success_envelope(
        &self,
        request: [u8; 16],
        fingerprint: [u8; 32],
        value: CanonicalValue,
    ) -> Result<Envelope, LiveError> {
        Ok(Envelope {
            request: Some(request),
            watch: None,
            message: Message::Result {
                status: ResultStatus::Success,
                value: Some(value),
                fingerprint,
                diagnostic: None,
            },
            extensions: BTreeMap::new(),
        })
    }

    fn live_snapshot(
        &self,
        request: [u8; 16],
        watch: [u8; 16],
        state: &ApplicationWatch,
    ) -> Envelope {
        Envelope {
            request: Some(request),
            watch: Some(watch),
            message: Message::Snapshot {
                revision: state.revision,
                present: state.present.clone(),
                snapshot: state.snapshot.clone(),
            },
            extensions: BTreeMap::new(),
        }
    }

    fn watch_value(&self, session: [u8; 16], source: &str) -> Result<PresentNode, LiveError> {
        let repl = self.repl_candidate(session)?;
        let value = repl
            .preview(source)
            .map_err(|_| LiveError::ApplicationRejected)?;
        // A generic typed-value node is the universal fallback: rendering
        // specializations may be added without making any value unwatchable.
        PresentNode::from_value(value).map_err(|_| LiveError::ApplicationRejected)
    }
}

impl LiveApplication for ApplicationLiveAdapter {
    fn eval(
        &mut self,
        session: [u8; 16],
        request: [u8; 16],
        message: &Message,
    ) -> std::result::Result<Envelope, LiveError> {
        if !self.is_module_entry(message) {
            let Message::Eval { source, .. } = message else {
                return Err(LiveError::ApplicationRejected);
            };
            let mut repl = self.repl_candidate(session)?;
            let result = repl.submit(source);
            let value = result
                .map_err(|error| {
                    LiveError::from(ApplicationError::Evaluation(error.code().to_owned()))
                })?
                .unwrap_or_else(|| CanonicalValue::new(OvbRaw::Null).expect("null is canonical"));
            self.publish_repl_candidate(session, repl);
            return self.success_envelope(request, eval_fingerprint(message)?, value);
        }
        let admitted = self.evaluate_eval_message(message)?;
        let value = self.authority.evaluate(&admitted, &Environment::new())?;
        self.success_envelope(request, eval_fingerprint(message)?, value)
    }

    fn eval_with_transaction<'a>(
        &'a mut self,
        session: [u8; 16],
        request: [u8; 16],
        message: &'a Message,
        context: Option<&'a RuntimeActivationContext>,
        _work: &'a mut LiveApplicationWorkLease,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<LiveEvalResponse, LiveError>> + 'a>> {
        Box::pin(async move {
            if !self.is_module_entry(message) {
                let Message::Eval { source, .. } = message else {
                    return Err(LiveError::ApplicationRejected);
                };
                let mut repl = self.repl_candidate(session)?;
                let staged = match repl.stage_activation(source) {
                    Ok(staged) => staged,
                    Err(error) if error.code() == "ORNA-REPL-EFFECT" => {
                        let value = repl
                            .submit(source)
                            .map_err(|error| {
                                LiveError::from(ApplicationError::Evaluation(
                                    error.code().to_owned(),
                                ))
                            })?
                            .unwrap_or_else(|| {
                                CanonicalValue::new(OvbRaw::Null).expect("null is canonical")
                            });
                        self.publish_repl_candidate(session, repl);
                        let envelope =
                            self.success_envelope(request, eval_fingerprint(message)?, value)?;
                        return Ok(LiveEvalResponse::pure(envelope));
                    }
                    Err(_) => return Err(LiveError::ApplicationRejected),
                };
                let mut handler = SourceMutationEffectHandler::new(self.repl_table_schemas());
                let (value, successor) = repl
                    .evaluate_staged_with_effects(staged, &mut handler)
                    .map_err(|_| LiveError::ApplicationRejected)?;
                let mutations = handler.into_mutations().map_err(LiveError::from)?;
                let value = value.unwrap_or_else(|| {
                    CanonicalValue::new(OvbRaw::Null).expect("null is canonical")
                });
                let envelope = self.success_envelope(request, eval_fingerprint(message)?, value)?;
                if mutations.is_empty() {
                    self.publish_repl_candidate(session, successor);
                    return Ok(LiveEvalResponse::pure(envelope));
                }
                let Some(context) = context else {
                    return Err(LiveError::ApplicationRejected);
                };
                let activation = self.authority.stage_mutations(context.clone(), mutations)?;
                let sessions = Arc::clone(&self.sessions);
                let transaction = LiveEvalTransaction::new(
                    activation.mutations().to_vec(),
                    activation.next_digest(),
                    Arc::new(NoFault),
                )
                .after_commit(move || {
                    sessions
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(session, ApplicationReplSession { repl: successor });
                });
                return Ok(LiveEvalResponse::transaction(envelope, transaction));
            }
            let admitted = self.evaluate_eval_message(message)?;
            let staged = self
                .authority
                .evaluate_staged(&admitted, &Environment::new())?;
            let envelope =
                self.success_envelope(request, eval_fingerprint(message)?, staged.value().clone())?;
            if staged.mutations().is_empty() {
                return Ok(LiveEvalResponse::pure(envelope));
            }
            // Fail closed: admitted source effects require an authoritative
            // staging context captured by the host before evaluation.
            let Some(context) = context else {
                return Err(LiveError::ApplicationRejected);
            };
            let activation = staged.stage(&self.authority, context)?;
            let transaction = LiveEvalTransaction::new(
                activation.mutations().to_vec(),
                activation.next_digest(),
                Arc::new(NoFault),
            );
            Ok(LiveEvalResponse::transaction(envelope, transaction))
        })
    }

    fn dispatch_eval_with_work<'a>(
        &'a mut self,
        session: [u8; 16],
        request: [u8; 16],
        message: &'a Message,
        context: Option<&'a RuntimeActivationContext>,
        work: &'a mut LiveApplicationWorkLease,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<LiveEvalResponse, LiveError>> + 'a>> {
        Box::pin(async move {
            work.check_active()?;
            let response = self
                .eval_with_transaction(session, request, message, context, work)
                .await?;
            work.complete();
            work.check_active()?;
            Ok(response)
        })
    }

    fn dispatch_eval_with_effects<'a>(
        &'a mut self,
        session: [u8; 16],
        request: [u8; 16],
        message: &'a Message,
        context: Option<&'a RuntimeActivationContext>,
        work: &'a mut LiveApplicationWorkLease,
        effects: Option<&'a dyn LiveAdminEffectDispatcher>,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<LiveEvalResponse, LiveError>> + 'a>> {
        Box::pin(async move {
            if !self.is_module_entry(message) {
                return self
                    .dispatch_eval_with_work(session, request, message, context, work)
                    .await;
            }
            let (Some(context), Some(effects)) = (context, effects) else {
                return self
                    .dispatch_eval_with_work(session, request, message, context, work)
                    .await;
            };
            work.check_active()?;
            let admitted = self.evaluate_eval_message(message)?;
            let dispatcher = LiveEffectAdapter(effects);
            let staged = self
                .authority
                .evaluate_staged_with_async_effects(
                    &admitted,
                    &Environment::new(),
                    context,
                    &dispatcher,
                )
                .await
                .map_err(LiveError::from)?;
            let envelope =
                self.success_envelope(request, eval_fingerprint(message)?, staged.value().clone())?;
            let response = if staged.mutations().is_empty() {
                LiveEvalResponse::pure(envelope)
            } else {
                let activation = staged.stage(&self.authority, context)?;
                let transaction = LiveEvalTransaction::new(
                    activation.mutations().to_vec(),
                    activation.next_digest(),
                    Arc::new(NoFault),
                );
                LiveEvalResponse::transaction(envelope, transaction)
            };
            work.complete();
            work.check_active()?;
            Ok(response)
        })
    }

    fn watch(
        &mut self,
        session: [u8; 16],
        request: [u8; 16],
        message: &Message,
    ) -> std::result::Result<Envelope, LiveError> {
        let Message::Watch {
            source, database, ..
        } = message
        else {
            return Err(LiveError::ApplicationRejected);
        };
        let Some((expected_database, runtime)) = self.runtime_identity else {
            return Err(LiveError::RuntimeUnavailable);
        };
        if database.database != expected_database
            || database.snapshot.as_ref().is_some_and(|snapshot| {
                !snapshot_matches_runtime(snapshot, expected_database, runtime)
            })
        {
            return Err(LiveError::ApplicationRejected);
        }
        let present = self.watch_value(session, source)?;
        let snapshot = database.snapshot.clone().map_or_else(
            || CanonicalSnapshot::cwd(database.database, runtime, 0.into()),
            Ok,
        )
        .map_err(|_| LiveError::ApplicationRejected)?;
        let mut watch = [0; 16];
        getrandom::fill(&mut watch).map_err(|_| LiveError::RuntimeUnavailable)?;
        let key = (session, watch);
        let mut watches = self
            .watches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if watches.len() >= 65_536 || watches.contains_key(&key) {
            return Err(LiveError::ApplicationRejected);
        }
        let state = ApplicationWatch {
            source: source.clone(),
            snapshot,
            present,
            revision: 0,
        };
        watches.insert(key, state.clone());
        Ok(self.live_snapshot(request, watch, &state))
    }

    fn resync(
        &mut self,
        session: [u8; 16],
        request: [u8; 16],
        watch: [u8; 16],
        message: &Message,
    ) -> std::result::Result<Envelope, LiveError> {
        if !matches!(message, Message::Resync) {
            return Err(LiveError::ApplicationRejected);
        }
        let key = (session, watch);
        let mut watches = self
            .watches
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = watches.get_mut(&key).ok_or(LiveError::ApplicationRejected)?;
        // This application adapter has no dependency scheduler yet. An
        // explicit resync therefore re-evaluates the read-only expression
        // and returns a complete root snapshot; a specialized delta is only
        // an optimization and cannot be required for a live value to refresh.
        let present = self.watch_value(session, &state.source)?;
        if present != state.present {
            state.revision = state
                .revision
                .checked_add(1)
                .ok_or(LiveError::ApplicationRejected)?;
            state.present = present;
        }
        Ok(self.live_snapshot(request, watch, state))
    }

    fn dispatch_with_work<'a>(
        &'a mut self,
        session: [u8; 16],
        request: [u8; 16],
        message: &'a Message,
        watch: Option<[u8; 16]>,
        fingerprint: [u8; 32],
        work: &'a mut LiveApplicationWorkLease,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<Envelope, LiveError>> + 'a>> {
        Box::pin(async move {
            work.check_active()?;
            let response = match message {
                Message::Watch { .. } => LiveApplication::watch(self, session, request, message),
                Message::Resync => LiveApplication::resync(
                    self,
                    session,
                    request,
                    watch.ok_or(LiveError::ApplicationRejected)?,
                    message,
                ),
                Message::Unsubscribe => {
                    if let Some(watch) = watch {
                        self.watches
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .remove(&(session, watch));
                    }
                    Ok(control_result(request, fingerprint))
                }
                Message::Cancel {
                    target_kind,
                    target,
                } => {
                    if *target_kind == orna_protocol_v1::TargetKind::Watch {
                        self.watches
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .remove(&(session, *target));
                    }
                    Ok(control_result(request, fingerprint))
                }
                _ => Err(LiveError::UnsupportedOperation),
            };
            work.complete();
            let response = response?;
            work.check_active()?;
            Ok(response)
        })
    }
}

fn control_result(request: [u8; 16], fingerprint: [u8; 32]) -> Envelope {
    Envelope {
        request: Some(request),
        watch: None,
        message: Message::Result {
            status: ResultStatus::Success,
            value: Some(CanonicalValue::unit()),
            fingerprint,
            diagnostic: None,
        },
        extensions: BTreeMap::new(),
    }
}

fn snapshot_matches_runtime(
    snapshot: &CanonicalSnapshot,
    expected_database: [u8; 16],
    expected_runtime: [u8; 16],
) -> bool {
    match snapshot {
        CanonicalSnapshot::Cwd {
            database, runtime, ..
        } => *database == expected_database && *runtime == expected_runtime,
        CanonicalSnapshot::Commit { database, .. } => *database == expected_database,
    }
}

/// Extracts the wire fingerprint from an Eval message; any other message shape
/// is rejected before evaluation.
fn eval_fingerprint(message: &Message) -> std::result::Result<[u8; 32], LiveError> {
    let Message::Eval { fingerprint, .. } = message else {
        return Err(LiveError::ApplicationRejected);
    };
    Ok(*fingerprint)
}

/// An immutable, fully admitted application program.
#[derive(Clone, Debug)]
pub struct AdmittedApplication {
    logical_path: String,
    source_digest: [u8; 32],
    requires_publication_metadata: bool,
    entry: String,
    functions: Functions,
    limits: Limits,
    module_header: orna_semantic_v1::ModuleHeader,
}

impl AdmittedApplication {
    /// Returns whether the admitted source names either runtime publication
    /// relation and needs a durable publication snapshot at evaluation.
    #[must_use]
    pub const fn requires_publication_metadata(&self) -> bool {
        self.requires_publication_metadata
    }

    #[must_use]
    pub fn logical_path(&self) -> &str {
        &self.logical_path
    }

    #[must_use]
    pub fn source_digest(&self) -> [u8; 32] {
        self.source_digest
    }

    #[must_use]
    pub fn entry(&self) -> &str {
        &self.entry
    }
}

fn put_bytes(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_admission_reaches_real_evaluator() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let source = include_str!("../tests/fixtures/source-admission-main.orna");
        let admitted = authority
            .admit_module("main.orna", source, "main")
            .expect("source should be admitted");
        let result = authority
            .evaluate(&admitted, &Environment::new())
            .expect("entry should execute");
        let expected = orna_foundation_v1::Value::new(orna_foundation_v1::OvbRaw::Int(
            num_bigint::BigInt::from(41_i64),
        ))
        .expect("canonical integer");
        assert_eq!(result, expected);
    }

    #[test]
    fn live_adapter_evaluates_eval_and_preserves_result_identity() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let mut adapter = ApplicationLiveAdapter::new(authority);
        let request = [8; 16];
        let fingerprint = [9; 32];
        let message = Message::Eval {
            source: include_str!("../tests/fixtures/source-admission-main.orna").to_owned(),
            database: orna_protocol_v1::DatabaseContext {
                database: [1; 16],
                snapshot: None,
            },
            presentation: orna_protocol_v1::PresentationContext {
                locale: "en-US".to_owned(),
                timezone: None,
                width: None,
                theme: "terminal/default".to_owned(),
                supported_kinds: Vec::new(),
            },
            fingerprint,
        };
        let response = LiveApplication::eval(&mut adapter, [7; 16], request, &message)
            .expect("Eval should be admitted and evaluated");
        assert_eq!(response.request, Some(request));
        assert_eq!(response.watch, None);
        let Message::Result {
            status,
            value,
            fingerprint: returned_fingerprint,
            diagnostic,
        } = response.message
        else {
            panic!("adapter must return a Result envelope");
        };
        assert_eq!(status, ResultStatus::Success);
        assert_eq!(returned_fingerprint, fingerprint);
        assert_eq!(diagnostic, None);
        assert_eq!(
            value,
            Some(
                orna_foundation_v1::Value::new(orna_foundation_v1::OvbRaw::Int(
                    num_bigint::BigInt::from(41_i64),
                ))
                .expect("canonical integer"),
            )
        );
    }

    #[test]
    fn live_watch_serves_a_pinned_typed_snapshot_and_resyncs_by_full_replacement() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let mut adapter =
            ApplicationLiveAdapter::new(authority).with_runtime_identity([1; 16], [2; 16]);
        let session = [3; 16];
        let request = [4; 16];
        let message = Message::Watch {
            source: include_str!("../tests/fixtures/live-watch-expression.orna")
                .trim()
                .to_owned(),
            database: orna_protocol_v1::DatabaseContext {
                database: [1; 16],
                snapshot: None,
            },
            presentation: orna_protocol_v1::PresentationContext {
                locale: "en-US".to_owned(),
                timezone: None,
                width: Some(800),
                theme: "web/default".to_owned(),
                supported_kinds: vec!["value".to_owned()],
            },
            refresh_floor: None,
        };
        let first = LiveApplication::watch(&mut adapter, session, request, &message)
            .expect("read-only watch should return a full snapshot");
        assert_eq!(first.request, Some(request));
        let Some(watch) = first.watch else {
            panic!("watch snapshots carry a session-scoped handle");
        };
        let expected_value = CanonicalValue::new(OvbRaw::Int(42.into()))
            .expect("canonical watched value");
        let Message::Snapshot {
            revision,
            present,
            snapshot,
        } = first.message
        else {
            panic!("a new watch starts with a complete snapshot");
        };
        assert_eq!(revision, 0);
        assert_eq!(present, PresentNode::from_value(expected_value).unwrap());
        assert_eq!(
            snapshot,
            CanonicalSnapshot::cwd([1; 16], [2; 16], 0.into()).unwrap()
        );

        let refreshed = LiveApplication::resync(
            &mut adapter,
            session,
            [5; 16],
            watch,
            &Message::Resync,
        )
        .expect("explicit resync returns a full replacement snapshot");
        assert_eq!(refreshed.request, Some([5; 16]));
        assert_eq!(refreshed.watch, Some(watch));
        assert_eq!(
            refreshed.message,
            Message::Snapshot {
                revision: 0,
                present: PresentNode::from_value(
                    CanonicalValue::new(OvbRaw::Int(42.into())).unwrap()
                )
                .unwrap(),
                snapshot: CanonicalSnapshot::cwd([1; 16], [2; 16], 0.into()).unwrap(),
            }
        );
    }

    #[test]
    fn live_watch_refuses_a_database_outside_its_deployed_runtime() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let mut adapter =
            ApplicationLiveAdapter::new(authority).with_runtime_identity([1; 16], [2; 16]);
        let message = Message::Watch {
            source: include_str!("../tests/fixtures/live-watch-expression.orna")
                .trim()
                .to_owned(),
            database: orna_protocol_v1::DatabaseContext {
                database: [9; 16],
                snapshot: None,
            },
            presentation: orna_protocol_v1::PresentationContext {
                locale: "en-US".to_owned(),
                timezone: None,
                width: None,
                theme: "web/default".to_owned(),
                supported_kinds: Vec::new(),
            },
            refresh_floor: None,
        };
        assert_eq!(
            LiveApplication::watch(&mut adapter, [3; 16], [4; 16], &message),
            Err(LiveError::ApplicationRejected)
        );

        let wrong_runtime_snapshot = Message::Watch {
            source: include_str!("../tests/fixtures/live-watch-expression.orna")
                .trim()
                .to_owned(),
            database: orna_protocol_v1::DatabaseContext {
                database: [1; 16],
                snapshot: Some(CanonicalSnapshot::cwd([1; 16], [9; 16], 0.into()).unwrap()),
            },
            presentation: orna_protocol_v1::PresentationContext {
                locale: "en-US".to_owned(),
                timezone: None,
                width: None,
                theme: "web/default".to_owned(),
                supported_kinds: Vec::new(),
            },
            refresh_floor: None,
        };
        assert_eq!(
            LiveApplication::watch(&mut adapter, [3; 16], [5; 16], &wrong_runtime_snapshot),
            Err(LiveError::ApplicationRejected)
        );
    }

    struct UnusedLiveAdminDispatcher;

    impl LiveAdminEffectDispatcher for UnusedLiveAdminDispatcher {
        fn pause_stream<'a>(
            &'a self,
            _stream: CanonicalValue,
            _reason: Option<String>,
            _context: &'a RuntimeActivationContext,
        ) -> Pin<Box<dyn Future<Output = std::result::Result<CanonicalValue, String>> + Send + 'a>>
        {
            Box::pin(async { Err("unexpected pause dispatch".to_owned()) })
        }
    }

    #[test]
    fn effect_dispatch_routes_non_module_eval_through_the_repl() {
        use futures::executor::block_on;
        use orna_live_v1::LiveApplicationWorkSupervisor;
        use orna_repository_v1::Repository;
        use orna_runtime_v1::{RuntimeIdentity, RuntimeState};

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock is after Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "orna-live-adapter-repl-dispatch-{}-{timestamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("temporary repository directory");
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .status()
            .expect("git init starts");
        assert!(status.success(), "git init creates the runtime repository");
        let repository = Repository::discover(&root).expect("temporary Git repository");
        let state = block_on(RuntimeState::open(
            &repository,
            RuntimeIdentity {
                database_id: [61; 16],
                repository_id: [62; 16],
            },
            [63; 32],
        ))
        .expect("runtime state opens");
        let context = block_on(state.begin_activation()).expect("activation context captures");
        let supervisor = LiveApplicationWorkSupervisor::new();
        let session = [64; 16];
        let request = [65; 16];
        let mut work = supervisor
            .admit(session, request)
            .expect("work lease admits");
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let mut adapter = ApplicationLiveAdapter::new(authority);
        let fingerprint = [66; 32];
        let message = eval_message(
            include_str!("../tests/fixtures/remote-repl-binding.orna"),
            fingerprint,
        );
        let dispatcher = UnusedLiveAdminDispatcher;

        let response = block_on(adapter.dispatch_eval_with_effects(
            session,
            request,
            &message,
            Some(&context),
            &mut work,
            Some(&dispatcher),
        ))
        .expect("effect-enabled host still dispatches REPL source");
        let LiveEvalResponse::Pure(envelope) = response else {
            panic!("a pure REPL declaration must not fabricate a transaction");
        };
        assert_eq!(envelope.request, Some(request));
        let Message::Result {
            status,
            value,
            fingerprint: returned_fingerprint,
            ..
        } = envelope.message
        else {
            panic!("REPL dispatch returns a Result envelope");
        };
        assert_eq!(status, ResultStatus::Success);
        assert_eq!(returned_fingerprint, fingerprint);
        assert_eq!(value, Some(CanonicalValue::new(OvbRaw::Null).unwrap()));

        drop(context);
        drop(state);
        std::fs::remove_dir_all(root).expect("temporary repository is removed");
    }

    fn eval_message(source: &str, fingerprint: [u8; 32]) -> Message {
        Message::Eval {
            source: source.to_owned(),
            database: orna_protocol_v1::DatabaseContext {
                database: [1; 16],
                snapshot: None,
            },
            presentation: orna_protocol_v1::PresentationContext {
                locale: "en-US".to_owned(),
                timezone: None,
                width: None,
                theme: "terminal/default".to_owned(),
                supported_kinds: Vec::new(),
            },
            fingerprint,
        }
    }

    #[test]
    fn remote_repl_retains_bindings_and_helpers_per_session() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let mut adapter = ApplicationLiveAdapter::new(authority);
        let session_a = [31; 16];
        let session_b = [32; 16];
        let binding = include_str!("../tests/fixtures/remote-repl-binding.orna");
        let helper = include_str!("../tests/fixtures/remote-repl-helper.orna");
        let import = include_str!("../tests/fixtures/remote-repl-wildcard-import.orna");
        let expression = include_str!("../tests/fixtures/remote-repl-use-session-state.orna");

        for (session, source) in [
            (session_a, import),
            (session_a, binding),
            (session_a, helper),
        ] {
            LiveApplication::eval(
                &mut adapter,
                session,
                [1; 16],
                &eval_message(source, [2; 32]),
            )
            .expect("REPL declarations should be admitted");
        }
        let result = LiveApplication::eval(
            &mut adapter,
            session_a,
            [3; 16],
            &eval_message(expression, [4; 32]),
        )
        .expect("same session should retain its declarations");
        assert!(matches!(
            result.message,
            Message::Result {
                status: ResultStatus::Success,
                value: Some(value),
                ..
            } if value == CanonicalValue::new(OvbRaw::Int(82.into())).expect("expected result")
        ));

        let isolated = LiveApplication::eval(
            &mut adapter,
            session_b,
            [5; 16],
            &eval_message(expression, [6; 32]),
        );
        assert!(matches!(isolated, Err(LiveError::ApplicationRejected)));
    }

    #[test]
    fn effectful_repl_candidate_stages_writes_without_publishing_early() {
        let standard_sources = reference_standard_sources().into_iter().collect::<Vec<_>>();
        let catalogue = Catalogue::authoritative_fixture()
            .with_standard_sources(&reference_standard_profile(), standard_sources.clone())
            .expect("reference standard modules should be admitted");
        let mut session = AdmittedReplSession::from_catalogue(
            &[],
            catalogue.clone(),
            standard_sources,
            Limits::default(),
        )
        .expect("admitted catalogue should initialize a session");
        session
            .submit(include_str!(
                "../tests/fixtures/remote-repl-contact-key.orna"
            ))
            .expect("session binding should be retained");
        session
            .submit(include_str!(
                "../tests/fixtures/remote-repl-contact-import.orna"
            ))
            .expect("ordinary module wildcard import should be admitted");
        let source = include_str!("../tests/fixtures/remote-repl-contact-insert.orna");
        let staged = session
            .stage_activation(source)
            .expect("table insert should be admitted");
        let tables = analyze_with_catalogue(&[], &catalogue)
            .modules
            .values()
            .flat_map(admitted_table_schemas)
            .collect();
        let mut effects = SourceMutationEffectHandler::new(tables);
        let (value, successor) = session
            .evaluate_staged_with_effects(staged, &mut effects)
            .expect("effect should evaluate only into a candidate session");
        let mutations = effects
            .into_mutations()
            .expect("candidate mutations should be canonical");
        assert_eq!(mutations.len(), 1);
        assert!(value.is_some());
        assert!(session.preview("key").is_ok());
        assert!(successor.preview("$_").is_ok());
    }

    fn note_source(body: &str) -> String {
        format!("pub table Note(id: Int) {{ text: Str, }} fn main() {{ {body} }}")
    }

    #[test]
    fn staged_evaluation_lowers_admitted_source_effects() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let admitted = authority
            .admit_module(
                "main.orna",
                note_source("Note.insert({ id: 7, text: \"once\" });"),
                "main",
            )
            .expect("source should be admitted");
        let staged = authority
            .evaluate_staged(&admitted, &Environment::new())
            .expect("admitted source effects should stage");
        assert_eq!(staged.mutations().len(), 1);
        let mutation = &staged.mutations()[0];
        assert_eq!(mutation.table(), "Note");
        assert!(mutation.is_insert());
        let key = orna_foundation_v1::Value::decode(mutation.key())
            .expect("mutation key must be canonical OVB");
        let expected_key = orna_foundation_v1::Value::new(orna_foundation_v1::OvbRaw::Int(
            num_bigint::BigInt::from(7_i64),
        ))
        .expect("canonical integer");
        assert_eq!(key, expected_key);
        let row = orna_foundation_v1::Value::decode(mutation.value().expect("row payload"))
            .expect("row payload must be canonical OVB");
        let orna_foundation_v1::OvbRaw::Map(fields) = row.raw() else {
            panic!("row payload must be a canonical record");
        };
        let text = fields.iter().find_map(|(key, value)| match key {
            orna_foundation_v1::OvbRaw::Text(name) if name == "text" => Some(value.clone()),
            _ => None,
        });
        assert_eq!(text, Some(orna_foundation_v1::OvbRaw::Text("once".into())));
    }

    #[test]
    fn staged_evaluation_lowers_admitted_delete_effects() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let admitted = authority
            .admit_module("main.orna", note_source("Note.delete(7);"), "main")
            .expect("source should be admitted");
        let staged = authority
            .evaluate_staged(&admitted, &Environment::new())
            .expect("admitted delete effect should stage");
        assert_eq!(staged.mutations().len(), 1);
        let mutation = &staged.mutations()[0];
        assert_eq!(mutation.table(), "Note");
        assert_eq!(mutation.value(), None);
        let key = orna_foundation_v1::Value::decode(mutation.key())
            .expect("mutation key must be canonical OVB");
        let expected_key = orna_foundation_v1::Value::new(orna_foundation_v1::OvbRaw::Int(
            num_bigint::BigInt::from(7_i64),
        ))
        .expect("canonical integer");
        assert_eq!(key, expected_key);
    }

    #[test]
    fn snapshot_mutation_overlay_updates_then_rekeys_one_row() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-update-rekey.orna",
                include_str!("../tests/fixtures/table-update-rekey.orna"),
                "main",
            )
            .expect("checked-in table mutation fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let key = int(1).encode().expect("key is canonical");
        let row = CanonicalValue::new(OvbRaw::Map(vec![
            (OvbRaw::Text("id".into()), OvbRaw::Int(1.into())),
            (OvbRaw::Text("text".into()), OvbRaw::Text("widget".into())),
            (OvbRaw::Text("quantity".into()), OvbRaw::Int(3.into())),
        ]))
        .expect("captured row is canonical")
        .encode()
        .expect("captured row encodes");
        let rows = BTreeMap::from([("Note".to_owned(), vec![(key.clone(), row)])]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");
        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("update and rekey should run against the pinned row");
        let mutations = handler
            .into_mutations()
            .expect("staged mutations should be valid");

        assert_eq!(mutations.len(), 2);
        assert_eq!(mutations[0].key(), key);
        assert_eq!(mutations[0].rekey_to(), None);
        let updated = CanonicalValue::decode(mutations[0].value().expect("updated row"))
            .expect("updated row is canonical");
        assert_eq!(updated, CanonicalValue::new(OvbRaw::Map(vec![
            (OvbRaw::Text("id".into()), OvbRaw::Int(1.into())),
            (OvbRaw::Text("text".into()), OvbRaw::Text("widget".into())),
            (OvbRaw::Text("quantity".into()), OvbRaw::Int(5.into())),
        ])).expect("expected row is canonical"));

        assert_eq!(mutations[1].key(), key);
        let new_key = int(2).encode().unwrap();
        assert_eq!(mutations[1].rekey_to(), Some(new_key.as_slice()));
        let rekeyed = CanonicalValue::decode(mutations[1].value().expect("re-keyed row"))
            .expect("re-keyed row is canonical");
        assert_eq!(rekeyed, CanonicalValue::new(OvbRaw::Map(vec![
            (OvbRaw::Text("id".into()), OvbRaw::Int(2.into())),
            (OvbRaw::Text("text".into()), OvbRaw::Text("widget".into())),
            (OvbRaw::Text("quantity".into()), OvbRaw::Int(5.into())),
        ])).expect("expected row is canonical"));
    }

    #[test]
    fn snapshot_rekey_key_reuse_keeps_each_rows_activation_order() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-key-reuse.orna",
                include_str!("../tests/fixtures/table-rekey-key-reuse.orna"),
                "main",
            )
            .expect("checked-in re-key fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("captured row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "first", 10).encode().unwrap()),
                (key(2), row(2, "second", 20).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");
        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("a re-keyed row frees its key for the next operation");

        let mutations = handler.into_mutations().expect("valid re-key log");
        assert_eq!(mutations.len(), 2);
        assert_eq!(mutations[0].key(), key(2));
        assert_eq!(mutations[0].rekey_to(), Some(key(3).as_slice()));
        assert_eq!(mutations[1].key(), key(1));
        assert_eq!(mutations[1].rekey_to(), Some(key(2).as_slice()));
        let first_identity = CanonicalValue::decode(mutations[1].value().unwrap()).unwrap();
        assert_eq!(first_identity, row(2, "first", 10));
        let second_identity = CanonicalValue::decode(mutations[0].value().unwrap()).unwrap();
        assert_eq!(second_identity, row(3, "second", 20));
    }

    #[test]
    fn reclaimed_source_key_retry_follows_current_row_identity() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-reclaimed-source-key-retry.orna",
                include_str!("../tests/fixtures/table-rekey-reclaimed-source-key-retry.orna"),
                "main",
            )
            .expect("checked-in reclaimed-source fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "source", 10).encode().unwrap()),
                (key(2), row(2, "destination", 20).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("reclaimed source key should resolve to its current row identity");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 6);

        assert_eq!(mutations[0].key(), key(2));
        assert_eq!(mutations[0].rekey_to(), Some(key(3).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[0].value().unwrap()).unwrap(),
            row(3, "destination", 20)
        );

        assert_eq!(mutations[1].key(), key(1));
        assert_eq!(mutations[1].rekey_to(), Some(key(4).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[1].value().unwrap()).unwrap(),
            row(4, "source", 10)
        );

        assert_eq!(mutations[2].key(), key(3));
        assert_eq!(mutations[2].rekey_to(), Some(key(1).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[2].value().unwrap()).unwrap(),
            row(1, "destination", 20)
        );

        assert_eq!(mutations[3].key(), key(1));
        assert_eq!(mutations[3].rekey_to(), Some(key(2).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[3].value().unwrap()).unwrap(),
            row(2, "destination", 20)
        );

        assert_eq!(mutations[4].key(), key(2));
        assert_eq!(mutations[4].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[4].value().unwrap()).unwrap(),
            row(2, "destination", 21)
        );

        assert_eq!(mutations[5].key(), key(4));
        assert_eq!(mutations[5].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[5].value().unwrap()).unwrap(),
            row(4, "source", 11)
        );
    }

    #[test]
    fn inserted_source_key_replacement_keeps_identity_through_rekey_tail() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-inserted-source-key-retry.orna",
                include_str!("../tests/fixtures/table-rekey-inserted-source-key-retry.orna"),
                "main",
            )
            .expect("checked-in inserted-source fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "source", 10).encode().unwrap()),
                (key(2), row(2, "destination", 20).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("a recovery insert can reclaim the source key for a later re-key");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 7);

        assert_eq!(mutations[0].key(), key(2));
        assert_eq!(mutations[0].rekey_to(), Some(key(3).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[0].value().unwrap()).unwrap(),
            row(3, "destination", 20)
        );

        assert_eq!(mutations[1].key(), key(1));
        assert_eq!(mutations[1].rekey_to(), Some(key(4).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[1].value().unwrap()).unwrap(),
            row(4, "source", 10)
        );

        assert_eq!(mutations[2].key(), key(1));
        assert!(mutations[2].is_insert());
        assert_eq!(mutations[2].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[2].value().unwrap()).unwrap(),
            row(1, "replacement", 30)
        );

        assert_eq!(mutations[3].key(), key(1));
        assert_eq!(mutations[3].rekey_to(), Some(key(2).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[3].value().unwrap()).unwrap(),
            row(2, "replacement", 30)
        );

        assert_eq!(mutations[4].key(), key(2));
        assert_eq!(mutations[4].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[4].value().unwrap()).unwrap(),
            row(2, "replacement", 31)
        );

        assert_eq!(mutations[5].key(), key(3));
        assert_eq!(mutations[5].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[5].value().unwrap()).unwrap(),
            row(3, "destination", 21)
        );

        assert_eq!(mutations[6].key(), key(4));
        assert_eq!(mutations[6].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[6].value().unwrap()).unwrap(),
            row(4, "source", 11)
        );
    }

    #[test]
    fn inserted_identity_update_is_carried_through_reclaimed_key_rekey() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-inserted-source-key-update-tail.orna",
                include_str!("../tests/fixtures/table-rekey-inserted-source-key-update-tail.orna"),
                "main",
            )
            .expect("checked-in inserted-update fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "source", 10).encode().unwrap()),
                (key(2), row(2, "destination", 20).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("the inserted identity keeps its update when it is re-keyed");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 8);

        assert_eq!(mutations[0].key(), key(2));
        assert_eq!(mutations[0].rekey_to(), Some(key(3).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[0].value().unwrap()).unwrap(),
            row(3, "destination", 20)
        );

        assert_eq!(mutations[1].key(), key(1));
        assert_eq!(mutations[1].rekey_to(), Some(key(4).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[1].value().unwrap()).unwrap(),
            row(4, "source", 10)
        );

        assert_eq!(mutations[2].key(), key(1));
        assert!(mutations[2].is_insert());
        assert_eq!(mutations[2].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[2].value().unwrap()).unwrap(),
            row(1, "replacement", 30)
        );

        assert_eq!(mutations[3].key(), key(1));
        assert_eq!(mutations[3].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[3].value().unwrap()).unwrap(),
            row(1, "replacement", 31)
        );

        assert_eq!(mutations[4].key(), key(1));
        assert_eq!(mutations[4].rekey_to(), Some(key(2).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[4].value().unwrap()).unwrap(),
            row(2, "replacement", 31)
        );

        assert_eq!(mutations[5].key(), key(2));
        assert_eq!(mutations[5].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[5].value().unwrap()).unwrap(),
            row(2, "replacement", 32)
        );

        assert_eq!(mutations[6].key(), key(3));
        assert_eq!(mutations[6].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[6].value().unwrap()).unwrap(),
            row(3, "destination", 21)
        );

        assert_eq!(mutations[7].key(), key(4));
        assert_eq!(mutations[7].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[7].value().unwrap()).unwrap(),
            row(4, "source", 11)
        );
    }

    #[test]
    fn inserted_row_recovery_update_survives_later_rekey() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-inserted-update-recovery-tail.orna",
                include_str!("../tests/fixtures/table-rekey-inserted-update-recovery-tail.orna"),
                "main",
            )
            .expect("checked-in inserted-update recovery fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "source", 10).encode().unwrap()),
                (key(2), row(2, "destination", 20).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("recovery updates remain attached to the inserted row through re-key");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 9);

        assert_eq!(mutations[0].key(), key(2));
        assert_eq!(mutations[0].rekey_to(), Some(key(3).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[0].value().unwrap()).unwrap(),
            row(3, "destination", 20)
        );

        assert_eq!(mutations[1].key(), key(1));
        assert_eq!(mutations[1].rekey_to(), Some(key(4).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[1].value().unwrap()).unwrap(),
            row(4, "source", 10)
        );

        assert_eq!(mutations[2].key(), key(1));
        assert!(mutations[2].is_insert());
        assert_eq!(mutations[2].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[2].value().unwrap()).unwrap(),
            row(1, "replacement", 30)
        );

        assert_eq!(mutations[3].key(), key(1));
        assert_eq!(mutations[3].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[3].value().unwrap()).unwrap(),
            row(1, "replacement", 31)
        );

        assert_eq!(mutations[4].key(), key(1));
        assert_eq!(mutations[4].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[4].value().unwrap()).unwrap(),
            row(1, "replacement", 32)
        );

        assert_eq!(mutations[5].key(), key(1));
        assert_eq!(mutations[5].rekey_to(), Some(key(2).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[5].value().unwrap()).unwrap(),
            row(2, "replacement", 32)
        );

        assert_eq!(mutations[6].key(), key(2));
        assert_eq!(mutations[6].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[6].value().unwrap()).unwrap(),
            row(2, "replacement", 33)
        );

        assert_eq!(mutations[7].key(), key(3));
        assert_eq!(mutations[7].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[7].value().unwrap()).unwrap(),
            row(3, "destination", 21)
        );

        assert_eq!(mutations[8].key(), key(4));
        assert_eq!(mutations[8].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[8].value().unwrap()).unwrap(),
            row(4, "source", 11)
        );
    }

    #[test]
    fn inserted_identity_keeps_recovery_update_across_later_rekey_retry() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-inserted-update-retry-tail.orna",
                include_str!("../tests/fixtures/table-rekey-inserted-update-retry-tail.orna"),
                "main",
            )
            .expect("checked-in inserted retry fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "source", 10).encode().unwrap()),
                (key(2), row(2, "destination", 20).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("the inserted identity survives a later failed re-key and retry");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 13);

        assert_eq!(mutations[0].key(), key(2));
        assert_eq!(mutations[0].rekey_to(), Some(key(3).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[0].value().unwrap()).unwrap(),
            row(3, "destination", 20)
        );

        assert_eq!(mutations[1].key(), key(1));
        assert_eq!(mutations[1].rekey_to(), Some(key(4).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[1].value().unwrap()).unwrap(),
            row(4, "source", 10)
        );

        assert_eq!(mutations[2].key(), key(1));
        assert!(mutations[2].is_insert());
        assert_eq!(mutations[2].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[2].value().unwrap()).unwrap(),
            row(1, "replacement", 30)
        );

        assert_eq!(mutations[3].key(), key(1));
        assert_eq!(mutations[3].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[3].value().unwrap()).unwrap(),
            row(1, "replacement", 31)
        );

        assert_eq!(mutations[4].key(), key(1));
        assert_eq!(mutations[4].rekey_to(), Some(key(2).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[4].value().unwrap()).unwrap(),
            row(2, "replacement", 31)
        );

        assert_eq!(mutations[5].key(), key(2));
        assert_eq!(mutations[5].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[5].value().unwrap()).unwrap(),
            row(2, "replacement", 32)
        );

        assert_eq!(mutations[6].key(), key(3));
        assert_eq!(mutations[6].rekey_to(), Some(key(5).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[6].value().unwrap()).unwrap(),
            row(5, "destination", 20)
        );

        assert_eq!(mutations[7].key(), key(5));
        assert_eq!(mutations[7].rekey_to(), Some(key(6).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[7].value().unwrap()).unwrap(),
            row(6, "destination", 20)
        );

        assert_eq!(mutations[8].key(), key(2));
        assert_eq!(mutations[8].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[8].value().unwrap()).unwrap(),
            row(2, "replacement", 33)
        );

        assert_eq!(mutations[9].key(), key(2));
        assert_eq!(mutations[9].rekey_to(), Some(key(5).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[9].value().unwrap()).unwrap(),
            row(5, "replacement", 33)
        );

        assert_eq!(mutations[10].key(), key(5));
        assert_eq!(mutations[10].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[10].value().unwrap()).unwrap(),
            row(5, "replacement", 34)
        );

        assert_eq!(mutations[11].key(), key(6));
        assert_eq!(mutations[11].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[11].value().unwrap()).unwrap(),
            row(6, "destination", 21)
        );

        assert_eq!(mutations[12].key(), key(4));
        assert_eq!(mutations[12].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[12].value().unwrap()).unwrap(),
            row(4, "source", 11)
        );
    }

    #[test]
    fn inserted_update_tracks_rekeyed_destination_occupant_through_retry() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-inserted-destination-update-tail.orna",
                include_str!("../tests/fixtures/table-rekey-inserted-destination-update-tail.orna"),
                "main",
            )
            .expect("checked-in inserted-destination fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "source", 10).encode().unwrap()),
                (key(2), row(2, "destination", 20).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("recovery moves the inserted destination occupant before retrying");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 12);

        let expected = [
            (2, Some(3), false, row(3, "destination", 20)),
            (1, Some(4), false, row(4, "source", 10)),
            (1, None, true, row(1, "replacement", 30)),
            (1, None, false, row(1, "replacement", 31)),
            (1, Some(2), false, row(2, "replacement", 31)),
            (2, None, false, row(2, "replacement", 32)),
            (2, Some(5), false, row(5, "replacement", 32)),
            (5, None, false, row(5, "replacement", 33)),
            (3, Some(2), false, row(2, "destination", 20)),
            (5, None, false, row(5, "replacement", 34)),
            (2, None, false, row(2, "destination", 21)),
            (4, None, false, row(4, "source", 11)),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            assert_eq!(
                CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                expected_row,
                "mutation {index} row value"
            );
        }
    }

    #[test]
    fn inserted_destination_update_survives_chained_rekey_retries() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-inserted-destination-update-chain.orna",
                include_str!("../tests/fixtures/table-rekey-inserted-destination-update-chain.orna"),
                "main",
            )
            .expect("checked-in inserted destination chain should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "source", 10).encode().unwrap()),
                (key(2), row(2, "destination", 20).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("the inserted destination row remains addressable through chained retries");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 15);

        let expected = [
            (2, Some(3), false, row(3, "destination", 20)),
            (1, Some(4), false, row(4, "source", 10)),
            (1, None, true, row(1, "replacement", 30)),
            (1, None, false, row(1, "replacement", 31)),
            (1, Some(2), false, row(2, "replacement", 31)),
            (2, None, false, row(2, "replacement", 32)),
            (2, Some(5), false, row(5, "replacement", 32)),
            (5, None, false, row(5, "replacement", 33)),
            (3, Some(2), false, row(2, "destination", 20)),
            (5, Some(6), false, row(6, "replacement", 33)),
            (6, None, false, row(6, "replacement", 34)),
            (2, Some(5), false, row(5, "destination", 20)),
            (5, None, false, row(5, "destination", 21)),
            (6, None, false, row(6, "replacement", 35)),
            (4, None, false, row(4, "source", 11)),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            assert_eq!(
                CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                expected_row,
                "mutation {index} row value"
            );
        }
    }

    #[test]
    fn rekey_survives_a_recovered_failed_update_and_later_mutation() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-recovery-tail.orna",
                include_str!("../tests/fixtures/table-rekey-recovery-tail.orna"),
                "main",
            )
            .expect("checked-in recovery fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("captured row is canonical")
        };
        let key = int(1).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![(key, row(1, "original", 3).encode().unwrap())],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("the failed update is recovered and the moved row remains addressable");

        let mutations = handler.into_mutations().expect("valid mutation log");
        assert_eq!(mutations.len(), 2);
        assert_eq!(mutations[0].key(), int(1).encode().unwrap());
        assert_eq!(mutations[0].rekey_to(), Some(int(2).encode().unwrap().as_slice()));
        assert_eq!(mutations[1].key(), int(2).encode().unwrap());
        assert_eq!(mutations[1].rekey_to(), None);
        let updated = CanonicalValue::decode(mutations[1].value().unwrap()).unwrap();
        assert_eq!(updated, row(2, "original", 9));
    }

    #[test]
    fn rekey_then_reuse_old_key_keeps_both_rows_addressable_through_updates() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-insert-reuse-update-tail.orna",
                include_str!("../tests/fixtures/table-rekey-insert-reuse-update-tail.orna"),
                "main",
            )
            .expect("checked-in key-reuse fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![(key(1), row(1, "original", 3).encode().unwrap())],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("replacement and re-keyed rows remain independently addressable");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 4);
        assert_eq!(mutations[0].key(), key(1));
        assert_eq!(mutations[0].rekey_to(), Some(key(2).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[0].value().unwrap()).unwrap(),
            row(2, "original", 3)
        );
        assert!(mutations[1].is_insert());
        assert_eq!(mutations[1].key(), key(1));
        assert_eq!(
            CanonicalValue::decode(mutations[1].value().unwrap()).unwrap(),
            row(1, "replacement", 4)
        );
        assert_eq!(mutations[2].key(), key(1));
        assert_eq!(mutations[2].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[2].value().unwrap()).unwrap(),
            row(1, "replacement", 5)
        );
        assert_eq!(mutations[3].key(), key(2));
        assert_eq!(mutations[3].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[3].value().unwrap()).unwrap(),
            row(2, "original", 6)
        );
    }

    #[test]
    fn recovered_duplicate_rekey_leaves_both_original_rows_available() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-duplicate-recovery-tail.orna",
                include_str!("../tests/fixtures/table-rekey-duplicate-recovery-tail.orna"),
                "main",
            )
            .expect("checked-in duplicate re-key fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "first", 3).encode().unwrap()),
                (key(2), row(2, "second", 4).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("recovery and later updates should see the unchanged source rows");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 2);
        assert_eq!(mutations[0].key(), key(1));
        assert_eq!(mutations[0].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[0].value().unwrap()).unwrap(),
            row(1, "first", 8)
        );
        assert_eq!(mutations[1].key(), key(2));
        assert_eq!(mutations[1].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[1].value().unwrap()).unwrap(),
            row(2, "second", 9)
        );
    }

    #[test]
    fn recovered_stale_source_rekey_preserves_the_latest_row_identity() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-stale-source-recovery-tail.orna",
                include_str!("../tests/fixtures/table-rekey-stale-source-recovery-tail.orna"),
                "main",
            )
            .expect("checked-in stale-source fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![(key(1), row(1, "original", 3).encode().unwrap())],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("recovery and later mutations should follow the last successful re-key");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 4);
        assert_eq!(mutations[0].key(), key(1));
        assert_eq!(mutations[0].rekey_to(), Some(key(2).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[0].value().unwrap()).unwrap(),
            row(2, "original", 3)
        );
        assert_eq!(mutations[1].key(), key(2));
        assert_eq!(mutations[1].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[1].value().unwrap()).unwrap(),
            row(2, "original", 8)
        );
        assert_eq!(mutations[2].key(), key(2));
        assert_eq!(mutations[2].rekey_to(), Some(key(3).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[2].value().unwrap()).unwrap(),
            row(3, "original", 8)
        );
        assert_eq!(mutations[3].key(), key(3));
        assert_eq!(mutations[3].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[3].value().unwrap()).unwrap(),
            row(3, "original", 9)
        );
    }

    #[test]
    fn recovered_occupied_destination_rekey_can_claim_the_key_after_it_is_freed() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-recovered-occupied-key-tail.orna",
                include_str!("../tests/fixtures/table-rekey-recovered-occupied-key-tail.orna"),
                "main",
            )
            .expect("checked-in occupied-key fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "first", 10).encode().unwrap()),
                (key(2), row(2, "second", 20).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("recovery preserves the source row until the destination is freed");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 5);
        assert_eq!(mutations[0].key(), key(1));
        assert_eq!(mutations[0].rekey_to(), Some(key(3).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[0].value().unwrap()).unwrap(),
            row(3, "first", 10)
        );
        assert_eq!(mutations[1].key(), key(3));
        assert_eq!(mutations[1].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[1].value().unwrap()).unwrap(),
            row(3, "first", 15)
        );
        assert_eq!(mutations[2].key(), key(2));
        assert_eq!(mutations[2].rekey_to(), Some(key(1).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[2].value().unwrap()).unwrap(),
            row(1, "second", 20)
        );
        assert_eq!(mutations[3].key(), key(3));
        assert_eq!(mutations[3].rekey_to(), Some(key(2).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[3].value().unwrap()).unwrap(),
            row(2, "first", 15)
        );
        assert_eq!(mutations[4].key(), key(2));
        assert_eq!(mutations[4].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[4].value().unwrap()).unwrap(),
            row(2, "first", 16)
        );
    }

    #[test]
    fn recovered_delete_allows_a_failed_destination_rekey_to_retry() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-delete-retry-tail.orna",
                include_str!("../tests/fixtures/table-rekey-delete-retry-tail.orna"),
                "main",
            )
            .expect("checked-in delete-and-retry fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "moving", 10).encode().unwrap()),
                (key(2), row(2, "conflict", 20).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("recovery deletes the conflict before retrying the re-key");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 4);
        assert_eq!(mutations[0].key(), key(2));
        assert_eq!(mutations[0].rekey_to(), None);
        assert_eq!(mutations[0].value(), None);
        assert_eq!(mutations[1].key(), key(1));
        assert_eq!(mutations[1].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[1].value().unwrap()).unwrap(),
            row(1, "moving", 8)
        );
        assert_eq!(mutations[2].key(), key(1));
        assert_eq!(mutations[2].rekey_to(), Some(key(2).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[2].value().unwrap()).unwrap(),
            row(2, "moving", 8)
        );
        assert_eq!(mutations[3].key(), key(2));
        assert_eq!(mutations[3].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[3].value().unwrap()).unwrap(),
            row(2, "moving", 9)
        );
    }

    #[test]
    fn recovered_replacement_keeps_destination_occupied_for_rekey_retry() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-replacement-retry-tail.orna",
                include_str!("../tests/fixtures/table-rekey-replacement-retry-tail.orna"),
                "main",
            )
            .expect("checked-in replacement-and-retry fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "moving", 10).encode().unwrap()),
                (key(2), row(2, "original occupant", 20).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("recovered replacement remains the occupied destination on retry");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 4);
        assert_eq!(mutations[0].key(), key(2));
        assert_eq!(mutations[0].rekey_to(), None);
        assert_eq!(mutations[0].value(), None);

        assert_eq!(mutations[1].key(), key(2));
        assert!(mutations[1].is_insert());
        assert_eq!(mutations[1].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[1].value().unwrap()).unwrap(),
            row(2, "replacement", 20)
        );

        assert_eq!(mutations[2].key(), key(1));
        assert_eq!(mutations[2].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[2].value().unwrap()).unwrap(),
            row(1, "moving", 11)
        );

        assert_eq!(mutations[3].key(), key(2));
        assert_eq!(mutations[3].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[3].value().unwrap()).unwrap(),
            row(2, "replacement", 21)
        );
    }

    #[test]
    fn recovered_inserted_destination_update_follows_recovery_rekey() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-recovered-inserted-destination-update.orna",
                include_str!("../tests/fixtures/table-rekey-recovered-inserted-destination-update.orna"),
                "main",
            )
            .expect("checked-in recovered inserted-destination fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "moving", 10).encode().unwrap()),
                (key(2), row(2, "original occupant", 20).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("the inserted destination update follows its recovery re-key");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 9);

        let expected = [
            (2, Some(3), false, row(3, "original occupant", 20)),
            (2, None, true, row(2, "replacement", 30)),
            (2, None, false, row(2, "replacement", 31)),
            (2, Some(4), false, row(4, "replacement", 31)),
            (4, None, false, row(4, "replacement", 32)),
            (1, Some(2), false, row(2, "moving", 10)),
            (2, None, false, row(2, "moving", 11)),
            (4, None, false, row(4, "replacement", 33)),
            (3, None, false, row(3, "original occupant", 21)),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            assert_eq!(
                CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                expected_row,
                "mutation {index} row value"
            );
        }
    }

    #[test]
    fn recovered_inserted_destination_survives_reclaimed_retry_key() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-recovered-inserted-destination-chain.orna",
                include_str!("../tests/fixtures/table-rekey-recovered-inserted-destination-chain.orna"),
                "main",
            )
            .expect("checked-in inserted-destination recovery chain should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "moving", 10).encode().unwrap()),
                (key(2), row(2, "original occupant", 20).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("the inserted row and reclaimed destination occupant survive retry tails");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 15);

        let expected = [
            (2, Some(3), false, row(3, "original occupant", 20)),
            (2, None, true, row(2, "replacement", 30)),
            (2, None, false, row(2, "replacement", 31)),
            (2, Some(4), false, row(4, "replacement", 31)),
            (4, None, false, row(4, "replacement", 32)),
            (1, Some(2), false, row(2, "moving", 10)),
            (4, Some(5), false, row(5, "replacement", 32)),
            (5, None, false, row(5, "replacement", 33)),
            (3, Some(4), false, row(4, "original occupant", 20)),
            (4, Some(6), false, row(6, "original occupant", 20)),
            (6, None, false, row(6, "original occupant", 21)),
            (2, Some(4), false, row(4, "moving", 10)),
            (4, None, false, row(4, "moving", 11)),
            (5, None, false, row(5, "replacement", 34)),
            (6, None, false, row(6, "original occupant", 22)),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            assert_eq!(
                CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                expected_row,
                "mutation {index} row value"
            );
        }
    }

    #[test]
    fn successive_inserted_destination_occupants_keep_updates_through_retry() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-successive-inserted-destinations.orna",
                include_str!("../tests/fixtures/table-rekey-successive-inserted-destinations.orna"),
                "main",
            )
            .expect("checked-in successive destination fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "moving", 10).encode().unwrap()),
                (key(2), row(2, "original occupant", 20).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("each recovery occupant preserves its own update before source retry");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 14);

        let expected = [
            (2, Some(3), false, row(3, "original occupant", 20)),
            (2, None, true, row(2, "first replacement", 30)),
            (2, None, false, row(2, "first replacement", 31)),
            (2, Some(4), false, row(4, "first replacement", 31)),
            (4, None, false, row(4, "first replacement", 32)),
            (2, None, true, row(2, "second replacement", 40)),
            (2, None, false, row(2, "second replacement", 41)),
            (2, Some(5), false, row(5, "second replacement", 41)),
            (5, None, false, row(5, "second replacement", 42)),
            (1, Some(2), false, row(2, "moving", 10)),
            (2, None, false, row(2, "moving", 11)),
            (4, None, false, row(4, "first replacement", 33)),
            (5, None, false, row(5, "second replacement", 43)),
            (3, None, false, row(3, "original occupant", 21)),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            assert_eq!(
                CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                expected_row,
                "mutation {index} row value"
            );
        }
    }

    #[test]
    fn recovery_rekey_frees_destination_for_source_retry() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-recovery-moves-destination.orna",
                include_str!("../tests/fixtures/table-rekey-recovery-moves-destination.orna"),
                "main",
            )
            .expect("checked-in recovery destination fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "source", 10).encode().unwrap()),
                (key(2), row(2, "destination", 20).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("source retry should see the destination freed by recovery");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 4);
        assert_eq!(mutations[0].key(), key(2));
        assert_eq!(mutations[0].rekey_to(), Some(key(3).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[0].value().unwrap()).unwrap(),
            row(3, "destination", 20)
        );

        assert_eq!(mutations[1].key(), key(1));
        assert_eq!(mutations[1].rekey_to(), Some(key(2).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[1].value().unwrap()).unwrap(),
            row(2, "source", 10)
        );

        assert_eq!(mutations[2].key(), key(2));
        assert_eq!(mutations[2].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[2].value().unwrap()).unwrap(),
            row(2, "source", 11)
        );

        assert_eq!(mutations[3].key(), key(3));
        assert_eq!(mutations[3].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[3].value().unwrap()).unwrap(),
            row(3, "destination", 21)
        );
    }

    #[test]
    fn recovered_destination_reuse_blocks_source_retry_without_losing_rows() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-recovery-reuses-destination.orna",
                include_str!("../tests/fixtures/table-rekey-recovery-reuses-destination.orna"),
                "main",
            )
            .expect("checked-in destination-reuse fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "source", 10).encode().unwrap()),
                (key(2), row(2, "original destination", 20).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("retry failure should preserve replacement and relocated rows");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 6);

        assert_eq!(mutations[0].key(), key(2));
        assert_eq!(mutations[0].rekey_to(), Some(key(3).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[0].value().unwrap()).unwrap(),
            row(3, "original destination", 20)
        );

        assert_eq!(mutations[1].key(), key(2));
        assert!(mutations[1].is_insert());
        assert_eq!(mutations[1].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[1].value().unwrap()).unwrap(),
            row(2, "replacement", 30)
        );

        assert_eq!(mutations[2].key(), key(1));
        assert_eq!(mutations[2].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[2].value().unwrap()).unwrap(),
            row(1, "source", 11)
        );

        assert_eq!(mutations[3].key(), key(1));
        assert_eq!(mutations[3].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[3].value().unwrap()).unwrap(),
            row(1, "source", 12)
        );

        assert_eq!(mutations[4].key(), key(2));
        assert_eq!(mutations[4].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[4].value().unwrap()).unwrap(),
            row(2, "replacement", 31)
        );

        assert_eq!(mutations[5].key(), key(3));
        assert_eq!(mutations[5].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[5].value().unwrap()).unwrap(),
            row(3, "original destination", 21)
        );
    }

    #[test]
    fn destination_retry_chain_tracks_each_recovered_occupant() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-retry-after-recovery-chain.orna",
                include_str!("../tests/fixtures/table-rekey-retry-after-recovery-chain.orna"),
                "main",
            )
            .expect("checked-in destination-retry chain should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "source", 10).encode().unwrap()),
                (key(2), row(2, "original destination", 20).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("the source retry succeeds after each recovered destination move");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 7);

        assert_eq!(mutations[0].key(), key(2));
        assert_eq!(mutations[0].rekey_to(), Some(key(3).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[0].value().unwrap()).unwrap(),
            row(3, "original destination", 20)
        );

        assert_eq!(mutations[1].key(), key(2));
        assert!(mutations[1].is_insert());
        assert_eq!(mutations[1].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[1].value().unwrap()).unwrap(),
            row(2, "replacement", 30)
        );

        assert_eq!(mutations[2].key(), key(2));
        assert_eq!(mutations[2].rekey_to(), Some(key(4).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[2].value().unwrap()).unwrap(),
            row(4, "replacement", 30)
        );

        assert_eq!(mutations[3].key(), key(1));
        assert_eq!(mutations[3].rekey_to(), Some(key(2).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[3].value().unwrap()).unwrap(),
            row(2, "source", 10)
        );

        assert_eq!(mutations[4].key(), key(2));
        assert_eq!(mutations[4].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[4].value().unwrap()).unwrap(),
            row(2, "source", 11)
        );

        assert_eq!(mutations[5].key(), key(3));
        assert_eq!(mutations[5].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[5].value().unwrap()).unwrap(),
            row(3, "original destination", 21)
        );

        assert_eq!(mutations[6].key(), key(4));
        assert_eq!(mutations[6].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[6].value().unwrap()).unwrap(),
            row(4, "replacement", 31)
        );
    }

    #[test]
    fn relocated_destination_identity_can_reclaim_key_and_block_retry_again() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-recycled-destination-retry.orna",
                include_str!("../tests/fixtures/table-rekey-recycled-destination-retry.orna"),
                "main",
            )
            .expect("checked-in recycled-destination fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "source", 10).encode().unwrap()),
                (key(2), row(2, "original destination", 20).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("latest recovered destination identity controls each retry");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 9);

        assert_eq!(mutations[0].key(), key(2));
        assert_eq!(mutations[0].rekey_to(), Some(key(3).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[0].value().unwrap()).unwrap(),
            row(3, "original destination", 20)
        );

        assert_eq!(mutations[1].key(), key(2));
        assert!(mutations[1].is_insert());
        assert_eq!(mutations[1].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[1].value().unwrap()).unwrap(),
            row(2, "replacement", 30)
        );

        assert_eq!(mutations[2].key(), key(2));
        assert_eq!(mutations[2].rekey_to(), Some(key(4).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[2].value().unwrap()).unwrap(),
            row(4, "replacement", 30)
        );

        assert_eq!(mutations[3].key(), key(3));
        assert_eq!(mutations[3].rekey_to(), Some(key(2).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[3].value().unwrap()).unwrap(),
            row(2, "original destination", 20)
        );

        assert_eq!(mutations[4].key(), key(2));
        assert_eq!(mutations[4].rekey_to(), Some(key(5).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[4].value().unwrap()).unwrap(),
            row(5, "original destination", 20)
        );

        assert_eq!(mutations[5].key(), key(1));
        assert_eq!(mutations[5].rekey_to(), Some(key(2).as_slice()));
        assert_eq!(
            CanonicalValue::decode(mutations[5].value().unwrap()).unwrap(),
            row(2, "source", 10)
        );

        assert_eq!(mutations[6].key(), key(2));
        assert_eq!(mutations[6].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[6].value().unwrap()).unwrap(),
            row(2, "source", 11)
        );

        assert_eq!(mutations[7].key(), key(5));
        assert_eq!(mutations[7].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[7].value().unwrap()).unwrap(),
            row(5, "original destination", 21)
        );

        assert_eq!(mutations[8].key(), key(4));
        assert_eq!(mutations[8].rekey_to(), None);
        assert_eq!(
            CanonicalValue::decode(mutations[8].value().unwrap()).unwrap(),
            row(4, "replacement", 31)
        );
    }

    #[test]
    fn reclaimed_destination_occurrence_keeps_each_update_across_retry() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-repeated-destination-occurrence-updates.orna",
                include_str!("../tests/fixtures/table-rekey-repeated-destination-occurrence-updates.orna"),
                "main",
            )
            .expect("checked-in repeated-destination fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "source", 10).encode().unwrap()),
                (key(2), row(2, "original destination", 20).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("the same relocated destination can be updated at each retry occurrence");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 12);

        let expected = [
            (2, Some(3), false, row(3, "original destination", 20)),
            (2, None, true, row(2, "replacement", 30)),
            (3, None, false, row(3, "original destination", 21)),
            (2, Some(4), false, row(4, "replacement", 30)),
            (3, Some(2), false, row(2, "original destination", 21)),
            (2, None, false, row(2, "original destination", 22)),
            (2, Some(5), false, row(5, "original destination", 22)),
            (5, None, false, row(5, "original destination", 23)),
            (1, Some(2), false, row(2, "source", 10)),
            (2, None, false, row(2, "source", 11)),
            (4, None, false, row(4, "replacement", 31)),
            (5, None, false, row(5, "original destination", 24)),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            assert_eq!(
                CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                expected_row,
                "mutation {index} row value"
            );
        }
    }

    #[test]
    fn inserted_destination_identity_reclaims_target_across_retries() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-inserted-destination-reclaims-target.orna",
                include_str!("../tests/fixtures/table-rekey-inserted-destination-reclaims-target.orna"),
                "main",
            )
            .expect("checked-in inserted destination reclaim fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "source", 10).encode().unwrap()),
                (key(2), row(2, "original destination", 20).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("the inserted destination identity may reclaim its target before retry");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 12);

        let expected = [
            (2, Some(3), false, row(3, "original destination", 20)),
            (2, None, true, row(2, "replacement", 30)),
            (2, None, false, row(2, "replacement", 31)),
            (2, Some(4), false, row(4, "replacement", 31)),
            (4, Some(2), false, row(2, "replacement", 31)),
            (2, None, false, row(2, "replacement", 32)),
            (2, Some(5), false, row(5, "replacement", 32)),
            (5, None, false, row(5, "replacement", 33)),
            (1, Some(2), false, row(2, "source", 10)),
            (2, None, false, row(2, "source", 11)),
            (5, None, false, row(5, "replacement", 34)),
            (3, None, false, row(3, "original destination", 21)),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            assert_eq!(
                CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                expected_row,
                "mutation {index} row value"
            );
        }
    }

    #[test]
    fn inserted_destination_update_spans_two_source_retry_episodes() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-inserted-destination-two-source-retries.orna",
                include_str!("../tests/fixtures/table-rekey-inserted-destination-two-source-retries.orna"),
                "main",
            )
            .expect("checked-in two-source retry fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "source", 10).encode().unwrap()),
                (key(2), row(2, "original destination", 20).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("the inserted blocker follows both sources through separate retry episodes");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 12);

        let expected = [
            (2, Some(3), false, row(3, "original destination", 20)),
            (2, None, true, row(2, "replacement", 30)),
            (2, None, false, row(2, "replacement", 31)),
            (2, Some(4), false, row(4, "replacement", 31)),
            (4, None, false, row(4, "replacement", 32)),
            (1, Some(2), false, row(2, "source", 10)),
            (4, Some(5), false, row(5, "replacement", 32)),
            (5, None, false, row(5, "replacement", 33)),
            (3, Some(4), false, row(4, "original destination", 20)),
            (2, None, false, row(2, "source", 11)),
            (4, None, false, row(4, "original destination", 21)),
            (5, None, false, row(5, "replacement", 34)),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            assert_eq!(
                CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                expected_row,
                "mutation {index} row value"
            );
        }
    }

    #[test]
    fn inserted_blocker_reclaims_destination_across_three_source_attempts() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-inserted-blocker-three-source-attempts.orna",
                include_str!("../tests/fixtures/table-rekey-inserted-blocker-three-source-attempts.orna"),
                "main",
            )
            .expect("checked-in three-source blocker fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "first source", 10).encode().unwrap()),
                (key(2), row(2, "original destination", 20).encode().unwrap()),
                (key(3), row(3, "second source", 30).encode().unwrap()),
                (key(5), row(5, "third source", 50).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("the same inserted blocker reclaims the target across three source attempts");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 11);

        let expected = [
            (2, Some(4), false, row(4, "original destination", 20)),
            (2, None, true, row(2, "replacement", 30)),
            (2, Some(6), false, row(6, "replacement", 30)),
            (6, Some(2), false, row(2, "replacement", 30)),
            (2, Some(7), false, row(7, "replacement", 30)),
            (7, Some(2), false, row(2, "replacement", 30)),
            (2, None, false, row(2, "replacement", 31)),
            (4, None, false, row(4, "original destination", 21)),
            (1, None, false, row(1, "first source", 11)),
            (3, None, false, row(3, "second source", 31)),
            (5, None, false, row(5, "third source", 51)),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            assert_eq!(
                CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                expected_row,
                "mutation {index} row value"
            );
        }
    }

    #[test]
    fn inserted_blocker_updates_survive_three_source_retry_tails() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-inserted-blocker-three-source-updates.orna",
                include_str!("../tests/fixtures/table-rekey-inserted-blocker-three-source-updates.orna"),
                "main",
            )
            .expect("checked-in three-source update fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "first source", 10).encode().unwrap()),
                (key(2), row(2, "original destination", 20).encode().unwrap()),
                (key(3), row(3, "second source", 30).encode().unwrap()),
                (key(5), row(5, "third source", 50).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("the inserted blocker retains each update as it reclaims the key");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 14);

        let expected = [
            (2, Some(4), false, row(4, "original destination", 20)),
            (2, None, true, row(2, "replacement", 30)),
            (2, Some(6), false, row(6, "replacement", 30)),
            (6, None, false, row(6, "replacement", 31)),
            (6, Some(2), false, row(2, "replacement", 31)),
            (2, None, false, row(2, "replacement", 32)),
            (2, Some(7), false, row(7, "replacement", 32)),
            (7, None, false, row(7, "replacement", 33)),
            (7, Some(2), false, row(2, "replacement", 33)),
            (2, None, false, row(2, "replacement", 34)),
            (4, None, false, row(4, "original destination", 21)),
            (1, None, false, row(1, "first source", 11)),
            (3, None, false, row(3, "second source", 31)),
            (5, None, false, row(5, "third source", 51)),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            assert_eq!(
                CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                expected_row,
                "mutation {index} row value"
            );
        }
    }

    #[test]
    fn source_updates_survive_three_blocker_retry_edges() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-source-updates-three-blocker-retries.orna",
                include_str!("../tests/fixtures/table-rekey-source-updates-three-blocker-retries.orna"),
                "main",
            )
            .expect("checked-in source-update retry fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "first source", 10).encode().unwrap()),
                (key(2), row(2, "original destination", 20).encode().unwrap()),
                (key(3), row(3, "second source", 30).encode().unwrap()),
                (key(5), row(5, "third source", 50).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("source updates remain attached through each failed target attempt");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 18);

        let expected = [
            (1, None, false, row(1, "first source", 11)),
            (2, Some(4), false, row(4, "original destination", 20)),
            (4, None, false, row(4, "original destination", 21)),
            (2, None, true, row(2, "replacement", 30)),
            (2, None, false, row(2, "replacement", 31)),
            (3, None, false, row(3, "second source", 31)),
            (2, Some(6), false, row(6, "replacement", 31)),
            (6, None, false, row(6, "replacement", 32)),
            (6, Some(2), false, row(2, "replacement", 32)),
            (2, None, false, row(2, "replacement", 33)),
            (5, None, false, row(5, "third source", 51)),
            (2, Some(7), false, row(7, "replacement", 33)),
            (7, None, false, row(7, "replacement", 34)),
            (7, Some(2), false, row(2, "replacement", 34)),
            (2, None, false, row(2, "replacement", 35)),
            (1, Some(8), false, row(8, "first source", 11)),
            (3, Some(9), false, row(9, "second source", 31)),
            (5, Some(10), false, row(10, "third source", 51)),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            assert_eq!(
                CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                expected_row,
                "mutation {index} row value"
            );
        }
    }

    #[test]
    fn source_updates_to_pending_rows_survive_other_retry_tails() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-pending-source-updates-retry-tails.orna",
                include_str!("../tests/fixtures/table-rekey-pending-source-updates-retry-tails.orna"),
                "main",
            )
            .expect("checked-in pending-source retry fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "first source", 10).encode().unwrap()),
                (key(2), row(2, "original destination", 20).encode().unwrap()),
                (key(3), row(3, "second source", 30).encode().unwrap()),
                (key(5), row(5, "third source", 50).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("pending sources retain updates made in later retry tails");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 21);

        let expected = [
            (1, None, false, row(1, "first source", 11)),
            (2, Some(4), false, row(4, "original destination", 20)),
            (4, None, false, row(4, "original destination", 21)),
            (2, None, true, row(2, "replacement", 30)),
            (2, None, false, row(2, "replacement", 31)),
            (1, None, false, row(1, "first source", 12)),
            (3, None, false, row(3, "second source", 31)),
            (2, Some(6), false, row(6, "replacement", 31)),
            (6, None, false, row(6, "replacement", 32)),
            (6, Some(2), false, row(2, "replacement", 32)),
            (2, None, false, row(2, "replacement", 33)),
            (1, None, false, row(1, "first source", 13)),
            (3, None, false, row(3, "second source", 32)),
            (5, None, false, row(5, "third source", 51)),
            (2, Some(7), false, row(7, "replacement", 33)),
            (7, None, false, row(7, "replacement", 34)),
            (7, Some(2), false, row(2, "replacement", 34)),
            (2, None, false, row(2, "replacement", 35)),
            (1, Some(8), false, row(8, "first source", 13)),
            (3, Some(9), false, row(9, "second source", 32)),
            (5, Some(10), false, row(10, "third source", 51)),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            assert_eq!(
                CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                expected_row,
                "mutation {index} row value"
            );
        }
    }

    #[test]
    fn rekeyed_source_and_reused_source_key_updates_survive_retry_tails() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-moved-source-reused-key-retry-tails.orna",
                include_str!("../tests/fixtures/table-rekey-moved-source-reused-key-retry-tails.orna"),
                "main",
            )
            .expect("checked-in moved-source retry fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "first source", 10).encode().unwrap()),
                (key(2), row(2, "original destination", 20).encode().unwrap()),
                (key(3), row(3, "second source", 30).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("updates remain attached to moved and reused-key rows across retries");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 23);

        let expected = [
            (2, Some(4), false, row(4, "original destination", 20)),
            (1, Some(2), false, row(2, "first source", 10)),
            (1, None, true, row(1, "replacement", 30)),
            (2, None, false, row(2, "first source", 11)),
            (1, None, false, row(1, "replacement", 31)),
            (2, None, false, row(2, "first source", 12)),
            (1, None, false, row(1, "replacement", 32)),
            (2, Some(6), false, row(6, "first source", 12)),
            (6, None, false, row(6, "first source", 13)),
            (6, Some(2), false, row(2, "first source", 13)),
            (2, None, false, row(2, "first source", 14)),
            (2, None, false, row(2, "first source", 15)),
            (1, None, false, row(1, "replacement", 33)),
            (2, Some(7), false, row(7, "first source", 15)),
            (7, None, false, row(7, "first source", 16)),
            (3, Some(2), false, row(2, "second source", 30)),
            (2, None, false, row(2, "second source", 31)),
            (7, None, false, row(7, "first source", 17)),
            (1, None, false, row(1, "replacement", 34)),
            (2, None, false, row(2, "second source", 32)),
            (7, Some(9), false, row(9, "first source", 17)),
            (1, Some(8), false, row(8, "replacement", 34)),
            (2, Some(5), false, row(5, "second source", 32)),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            assert_eq!(
                CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                expected_row,
                "mutation {index} row value"
            );
        }
    }

    #[test]
    fn moved_source_updates_survive_retry_key_reuse_closure() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-moved-source-retry-key-reuse-closure.orna",
                include_str!("../tests/fixtures/table-rekey-moved-source-retry-key-reuse-closure.orna"),
                "main",
            )
            .expect("checked-in source-key closure fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "first source", 10).encode().unwrap()),
                (key(2), row(2, "original destination", 20).encode().unwrap()),
                (key(3), row(3, "second source", 30).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("the moved source closes the reused-key cycle after a retry");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 17);

        let expected = [
            (2, Some(4), false, row(4, "original destination", 20)),
            (1, Some(2), false, row(2, "first source", 10)),
            (2, None, false, row(2, "first source", 11)),
            (1, None, true, row(1, "replacement", 30)),
            (1, None, false, row(1, "replacement", 31)),
            (2, None, false, row(2, "first source", 12)),
            (1, None, false, row(1, "replacement", 32)),
            (2, Some(6), false, row(6, "first source", 12)),
            (6, None, false, row(6, "first source", 13)),
            (3, Some(2), false, row(2, "second source", 30)),
            (2, None, false, row(2, "second source", 31)),
            (1, Some(8), false, row(8, "replacement", 32)),
            (6, Some(1), false, row(1, "first source", 13)),
            (1, None, false, row(1, "first source", 14)),
            (1, Some(9), false, row(9, "first source", 14)),
            (9, None, false, row(9, "first source", 15)),
            (2, Some(5), false, row(5, "second source", 31)),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            assert_eq!(
                CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                expected_row,
                "mutation {index} row value"
            );
        }
    }

    #[test]
    fn deleted_replacement_key_reuse_survives_retry_closure_edges() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-retry-key-reuse-delete-closure.orna",
                include_str!("../tests/fixtures/table-rekey-retry-key-reuse-delete-closure.orna"),
                "main",
            )
            .expect("checked-in delete-and-reclaim fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "first source", 10).encode().unwrap()),
                (key(2), row(2, "original destination", 20).encode().unwrap()),
                (key(3), row(3, "second source", 30).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("delete, reclaim, and later key reuse preserve row identity through retries");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 19);

        let expected = [
            (2, Some(4), false, Some(row(4, "original destination", 20))),
            (1, Some(2), false, Some(row(2, "first source", 10))),
            (1, None, true, Some(row(1, "replacement", 30))),
            (2, None, false, Some(row(2, "first source", 11))),
            (1, None, false, Some(row(1, "replacement", 31))),
            (2, None, false, Some(row(2, "first source", 12))),
            (1, None, false, None),
            (2, Some(1), false, Some(row(1, "first source", 12))),
            (1, None, false, Some(row(1, "first source", 13))),
            (3, Some(2), false, Some(row(2, "second source", 30))),
            (2, None, false, Some(row(2, "second source", 31))),
            (2, None, false, Some(row(2, "second source", 32))),
            (1, Some(8), false, Some(row(8, "first source", 13))),
            (4, Some(1), false, Some(row(1, "original destination", 20))),
            (1, None, false, Some(row(1, "original destination", 21))),
            (2, Some(4), false, Some(row(4, "second source", 32))),
            (4, None, false, Some(row(4, "second source", 33))),
            (8, Some(2), false, Some(row(2, "first source", 13))),
            (2, None, false, Some(row(2, "first source", 14))),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            match expected_row {
                Some(expected_row) => assert_eq!(
                    CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                    expected_row,
                    "mutation {index} row value"
                ),
                None => assert_eq!(mutation.value(), None, "mutation {index} deletion value"),
            }
        }
    }

    #[test]
    fn deleting_moved_source_preserves_later_retry_key_owners() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-delete-moved-source-reuse-final-tail.orna",
                include_str!("../tests/fixtures/table-rekey-delete-moved-source-reuse-final-tail.orna"),
                "main",
            )
            .expect("checked-in moved-source deletion fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "first source", 10).encode().unwrap()),
                (key(2), row(2, "original destination", 20).encode().unwrap()),
                (key(3), row(3, "second source", 30).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("deleting a moved source leaves later retry key owners distinct");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 17);

        let expected = [
            (2, Some(4), false, Some(row(4, "original destination", 20))),
            (1, Some(2), false, Some(row(2, "first source", 10))),
            (1, None, true, Some(row(1, "replacement", 30))),
            (2, None, false, Some(row(2, "first source", 11))),
            (1, None, false, Some(row(1, "replacement", 31))),
            (2, None, false, None),
            (1, None, false, Some(row(1, "replacement", 32))),
            (1, None, false, None),
            (3, Some(2), false, Some(row(2, "second source", 30))),
            (1, None, true, Some(row(1, "late replacement", 40))),
            (1, None, false, Some(row(1, "late replacement", 41))),
            (2, None, false, Some(row(2, "second source", 31))),
            (1, None, false, None),
            (4, Some(1), false, Some(row(1, "original destination", 20))),
            (1, None, false, Some(row(1, "original destination", 21))),
            (2, Some(4), false, Some(row(4, "second source", 31))),
            (4, None, false, Some(row(4, "second source", 32))),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            match expected_row {
                Some(expected_row) => assert_eq!(
                    CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                    expected_row,
                    "mutation {index} row value"
                ),
                None => assert_eq!(mutation.value(), None, "mutation {index} deletion value"),
            }
        }
    }

    #[test]
    fn returned_source_delete_and_retry_reuse_key_in_order() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-returned-source-delete-reuse-final.orna",
                include_str!("../tests/fixtures/table-rekey-returned-source-delete-reuse-final.orna"),
                "main",
            )
            .expect("checked-in returned-source deletion fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "first source", 10).encode().unwrap()),
                (key(2), row(2, "original destination", 20).encode().unwrap()),
                (key(3), row(3, "second source", 30).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("returned-source deletion and a later retry keep reused-key owners distinct");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 20);

        let expected = [
            (2, Some(4), false, Some(row(4, "original destination", 20))),
            (1, Some(2), false, Some(row(2, "first source", 10))),
            (1, None, true, Some(row(1, "replacement", 30))),
            (2, None, false, Some(row(2, "first source", 11))),
            (1, None, false, Some(row(1, "replacement", 31))),
            (2, None, false, Some(row(2, "first source", 12))),
            (1, None, false, None),
            (2, Some(1), false, Some(row(1, "first source", 12))),
            (1, None, false, Some(row(1, "first source", 13))),
            (3, Some(2), false, Some(row(2, "second source", 30))),
            (2, None, false, Some(row(2, "second source", 31))),
            (1, None, false, Some(row(1, "first source", 14))),
            (1, None, false, None),
            (1, None, true, Some(row(1, "late replacement", 40))),
            (1, None, false, Some(row(1, "late replacement", 41))),
            (1, None, false, None),
            (4, Some(1), false, Some(row(1, "original destination", 20))),
            (1, None, false, Some(row(1, "original destination", 21))),
            (2, Some(4), false, Some(row(4, "second source", 31))),
            (4, None, false, Some(row(4, "second source", 32))),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            match expected_row {
                Some(expected_row) => assert_eq!(
                    CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                    expected_row,
                    "mutation {index} row value"
                ),
                None => assert_eq!(mutation.value(), None, "mutation {index} deletion value"),
            }
        }
    }

    #[test]
    fn returned_source_delete_reuse_by_competitor_allows_final_retry() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-returned-source-delete-competitor-closure.orna",
                include_str!("../tests/fixtures/table-rekey-returned-source-delete-competitor-closure.orna"),
                "main",
            )
            .expect("checked-in returned-source competitor fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "first source", 10).encode().unwrap()),
                (key(2), row(2, "original destination", 20).encode().unwrap()),
                (key(3), row(3, "second source", 30).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("a pending competitor moves aside so the final retry can claim the key");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 22);

        let expected = [
            (2, Some(4), false, Some(row(4, "original destination", 20))),
            (1, Some(2), false, Some(row(2, "first source", 10))),
            (1, None, true, Some(row(1, "replacement", 30))),
            (2, None, false, Some(row(2, "first source", 11))),
            (1, None, false, Some(row(1, "replacement", 31))),
            (2, None, false, Some(row(2, "first source", 12))),
            (1, None, false, None),
            (2, Some(1), false, Some(row(1, "first source", 12))),
            (1, None, false, Some(row(1, "first source", 13))),
            (3, Some(2), false, Some(row(2, "second source", 30))),
            (2, None, false, Some(row(2, "second source", 31))),
            (1, None, false, Some(row(1, "first source", 14))),
            (1, None, false, None),
            (2, Some(1), false, Some(row(1, "second source", 31))),
            (1, None, false, Some(row(1, "second source", 32))),
            (1, None, false, Some(row(1, "second source", 33))),
            (1, Some(5), false, Some(row(5, "second source", 33))),
            (5, None, false, Some(row(5, "second source", 34))),
            (4, Some(1), false, Some(row(1, "original destination", 20))),
            (1, None, false, Some(row(1, "original destination", 21))),
            (5, Some(2), false, Some(row(2, "second source", 34))),
            (2, None, false, Some(row(2, "second source", 35))),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            match expected_row {
                Some(expected_row) => assert_eq!(
                    CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                    expected_row,
                    "mutation {index} row value"
                ),
                None => assert_eq!(mutation.value(), None, "mutation {index} deletion value"),
            }
        }
    }

    #[test]
    fn returned_source_delete_competitor_deletion_allows_final_retry() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-returned-source-delete-competitor-deletion.orna",
                include_str!("../tests/fixtures/table-rekey-returned-source-delete-competitor-deletion.orna"),
                "main",
            )
            .expect("checked-in returned-source competitor deletion fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "first source", 10).encode().unwrap()),
                (key(2), row(2, "original destination", 20).encode().unwrap()),
                (key(3), row(3, "second source", 30).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("deleting the key competitor allows the final source retry");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 19);

        let expected = [
            (2, Some(4), false, Some(row(4, "original destination", 20))),
            (1, Some(2), false, Some(row(2, "first source", 10))),
            (1, None, true, Some(row(1, "replacement", 30))),
            (2, None, false, Some(row(2, "first source", 11))),
            (1, None, false, Some(row(1, "replacement", 31))),
            (2, None, false, Some(row(2, "first source", 12))),
            (1, None, false, None),
            (2, Some(1), false, Some(row(1, "first source", 12))),
            (1, None, false, Some(row(1, "first source", 13))),
            (3, Some(2), false, Some(row(2, "second source", 30))),
            (2, None, false, Some(row(2, "second source", 31))),
            (1, None, false, Some(row(1, "first source", 14))),
            (1, None, false, None),
            (2, Some(1), false, Some(row(1, "second source", 31))),
            (1, None, false, Some(row(1, "second source", 32))),
            (1, None, false, Some(row(1, "second source", 33))),
            (1, None, false, None),
            (4, Some(1), false, Some(row(1, "original destination", 20))),
            (1, None, false, Some(row(1, "original destination", 21))),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            match expected_row {
                Some(expected_row) => assert_eq!(
                    CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                    expected_row,
                    "mutation {index} row value"
                ),
                None => assert_eq!(mutation.value(), None, "mutation {index} deletion value"),
            }
        }
    }

    #[test]
    fn returned_key_competitor_return_then_final_owner_delete_closes_retry() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-returned-key-competitor-return-retry-closure.orna",
                include_str!("../tests/fixtures/table-rekey-returned-key-competitor-return-retry-closure.orna"),
                "main",
            )
            .expect("checked-in returned-key competitor closure fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "first source", 10).encode().unwrap()),
                (key(2), row(2, "original destination", 20).encode().unwrap()),
                (key(3), row(3, "second source", 30).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("a returned-key competitor can return, retry, and release the final owner");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 24);

        let expected = [
            (2, Some(4), false, Some(row(4, "original destination", 20))),
            (1, Some(2), false, Some(row(2, "first source", 10))),
            (1, None, true, Some(row(1, "replacement", 30))),
            (2, None, false, Some(row(2, "first source", 11))),
            (1, None, false, Some(row(1, "replacement", 31))),
            (2, None, false, Some(row(2, "first source", 12))),
            (1, None, false, None),
            (2, Some(1), false, Some(row(1, "first source", 12))),
            (1, None, false, Some(row(1, "first source", 13))),
            (3, Some(2), false, Some(row(2, "second source", 30))),
            (2, None, false, Some(row(2, "second source", 31))),
            (1, None, false, Some(row(1, "first source", 14))),
            (1, None, false, None),
            (2, Some(1), false, Some(row(1, "second source", 31))),
            (1, None, false, Some(row(1, "second source", 32))),
            (1, None, false, Some(row(1, "second source", 33))),
            (1, Some(2), false, Some(row(2, "second source", 33))),
            (2, None, false, Some(row(2, "second source", 34))),
            (4, Some(1), false, Some(row(1, "original destination", 20))),
            (1, None, false, Some(row(1, "original destination", 21))),
            (1, None, false, Some(row(1, "original destination", 22))),
            (1, None, false, None),
            (2, Some(1), false, Some(row(1, "second source", 34))),
            (1, None, false, Some(row(1, "second source", 35))),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            match expected_row {
                Some(expected_row) => assert_eq!(
                    CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                    expected_row,
                    "mutation {index} row value"
                ),
                None => assert_eq!(mutation.value(), None, "mutation {index} deletion value"),
            }
        }
    }

    #[test]
    fn returned_key_competitor_retry_after_final_owner_rekeys_away() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-returned-key-competitor-retry-owner-moves.orna",
                include_str!("../tests/fixtures/table-rekey-returned-key-competitor-retry-owner-moves.orna"),
                "main",
            )
            .expect("checked-in returned-key retry owner fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "first source", 10).encode().unwrap()),
                (key(2), row(2, "original destination", 20).encode().unwrap()),
                (key(3), row(3, "second source", 30).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("the competitor retries after the final owner re-keys away");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 27);

        let expected = [
            (2, Some(4), false, Some(row(4, "original destination", 20))),
            (1, Some(2), false, Some(row(2, "first source", 10))),
            (1, None, true, Some(row(1, "replacement", 30))),
            (2, None, false, Some(row(2, "first source", 11))),
            (1, None, false, Some(row(1, "replacement", 31))),
            (2, None, false, Some(row(2, "first source", 12))),
            (1, None, false, None),
            (2, Some(1), false, Some(row(1, "first source", 12))),
            (1, None, false, Some(row(1, "first source", 13))),
            (3, Some(2), false, Some(row(2, "second source", 30))),
            (2, None, false, Some(row(2, "second source", 31))),
            (1, None, false, Some(row(1, "first source", 14))),
            (1, None, false, None),
            (2, Some(1), false, Some(row(1, "second source", 31))),
            (1, None, false, Some(row(1, "second source", 32))),
            (1, None, false, Some(row(1, "second source", 33))),
            (1, Some(2), false, Some(row(2, "second source", 33))),
            (2, None, false, Some(row(2, "second source", 34))),
            (4, Some(1), false, Some(row(1, "original destination", 20))),
            (1, None, false, Some(row(1, "original destination", 21))),
            (1, None, false, Some(row(1, "original destination", 22))),
            (1, Some(5), false, Some(row(5, "original destination", 22))),
            (5, None, false, Some(row(5, "original destination", 23))),
            (2, Some(1), false, Some(row(1, "second source", 34))),
            (1, None, false, Some(row(1, "second source", 35))),
            (5, Some(4), false, Some(row(4, "original destination", 23))),
            (4, None, false, Some(row(4, "original destination", 24))),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            match expected_row {
                Some(expected_row) => assert_eq!(
                    CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                    expected_row,
                    "mutation {index} row value"
                ),
                None => assert_eq!(mutation.value(), None, "mutation {index} deletion value"),
            }
        }
    }

    #[test]
    fn returned_key_owner_return_retry_after_competitor_rekeys_into_source_key() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-returned-key-competitor-claims-return-key.orna",
                include_str!("../tests/fixtures/table-rekey-returned-key-competitor-claims-return-key.orna"),
                "main",
            )
            .expect("checked-in returned-key ownership retry fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "first source", 10).encode().unwrap()),
                (key(2), row(2, "original destination", 20).encode().unwrap()),
                (key(3), row(3, "second source", 30).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("the owner can retry its source-key return after the competitor moves aside");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 32);

        let expected = [
            (2, Some(4), false, Some(row(4, "original destination", 20))),
            (1, Some(2), false, Some(row(2, "first source", 10))),
            (1, None, true, Some(row(1, "replacement", 30))),
            (2, None, false, Some(row(2, "first source", 11))),
            (1, None, false, Some(row(1, "replacement", 31))),
            (2, None, false, Some(row(2, "first source", 12))),
            (1, None, false, None),
            (2, Some(1), false, Some(row(1, "first source", 12))),
            (1, None, false, Some(row(1, "first source", 13))),
            (3, Some(2), false, Some(row(2, "second source", 30))),
            (2, None, false, Some(row(2, "second source", 31))),
            (1, None, false, Some(row(1, "first source", 14))),
            (1, None, false, None),
            (2, Some(1), false, Some(row(1, "second source", 31))),
            (1, None, false, Some(row(1, "second source", 32))),
            (1, None, false, Some(row(1, "second source", 33))),
            (1, Some(2), false, Some(row(2, "second source", 33))),
            (2, None, false, Some(row(2, "second source", 34))),
            (4, Some(1), false, Some(row(1, "original destination", 20))),
            (1, None, false, Some(row(1, "original destination", 21))),
            (1, None, false, Some(row(1, "original destination", 22))),
            (1, Some(5), false, Some(row(5, "original destination", 22))),
            (5, None, false, Some(row(5, "original destination", 23))),
            (2, Some(1), false, Some(row(1, "second source", 34))),
            (1, None, false, Some(row(1, "second source", 35))),
            (1, Some(4), false, Some(row(4, "second source", 35))),
            (4, None, false, Some(row(4, "second source", 36))),
            (4, None, false, Some(row(4, "second source", 37))),
            (4, Some(6), false, Some(row(6, "second source", 37))),
            (6, None, false, Some(row(6, "second source", 38))),
            (5, Some(4), false, Some(row(4, "original destination", 23))),
            (4, None, false, Some(row(4, "original destination", 25))),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            match expected_row {
                Some(expected_row) => assert_eq!(
                    CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                    expected_row,
                    "mutation {index} row value"
                ),
                None => assert_eq!(mutation.value(), None, "mutation {index} deletion value"),
            }
        }
    }

    #[test]
    fn owner_retry_waits_for_competitor_target_closure() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-competitor-target-blocked-owner-retry-tail.orna",
                include_str!("../tests/fixtures/table-rekey-competitor-target-blocked-owner-retry-tail.orna"),
                "main",
            )
            .expect("checked-in nested competitor closure fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "first source", 10).encode().unwrap()),
                (key(2), row(2, "original destination", 20).encode().unwrap()),
                (key(3), row(3, "second source", 30).encode().unwrap()),
                (key(6), row(6, "move target blocker", 40).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("the owner retries after the competitor's blocked move closes");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 34);

        let expected = [
            (2, Some(4), false, Some(row(4, "original destination", 20))),
            (1, Some(2), false, Some(row(2, "first source", 10))),
            (1, None, true, Some(row(1, "replacement", 30))),
            (2, None, false, Some(row(2, "first source", 11))),
            (1, None, false, Some(row(1, "replacement", 31))),
            (2, None, false, Some(row(2, "first source", 12))),
            (1, None, false, None),
            (2, Some(1), false, Some(row(1, "first source", 12))),
            (1, None, false, Some(row(1, "first source", 13))),
            (3, Some(2), false, Some(row(2, "second source", 30))),
            (2, None, false, Some(row(2, "second source", 31))),
            (1, None, false, Some(row(1, "first source", 14))),
            (1, None, false, None),
            (2, Some(1), false, Some(row(1, "second source", 31))),
            (1, None, false, Some(row(1, "second source", 32))),
            (1, None, false, Some(row(1, "second source", 33))),
            (1, Some(2), false, Some(row(2, "second source", 33))),
            (2, None, false, Some(row(2, "second source", 34))),
            (4, Some(1), false, Some(row(1, "original destination", 20))),
            (1, None, false, Some(row(1, "original destination", 21))),
            (1, None, false, Some(row(1, "original destination", 22))),
            (1, Some(5), false, Some(row(5, "original destination", 22))),
            (5, None, false, Some(row(5, "original destination", 23))),
            (2, Some(1), false, Some(row(1, "second source", 34))),
            (1, None, false, Some(row(1, "second source", 35))),
            (1, Some(4), false, Some(row(4, "second source", 35))),
            (4, None, false, Some(row(4, "second source", 36))),
            (4, None, false, Some(row(4, "second source", 37))),
            (6, None, false, Some(row(6, "move target blocker", 41))),
            (6, None, false, None),
            (4, Some(6), false, Some(row(6, "second source", 37))),
            (6, None, false, Some(row(6, "second source", 38))),
            (5, Some(4), false, Some(row(4, "original destination", 23))),
            (4, None, false, Some(row(4, "original destination", 25))),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            match expected_row {
                Some(expected_row) => assert_eq!(
                    CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                    expected_row,
                    "mutation {index} row value"
                ),
                None => assert_eq!(mutation.value(), None, "mutation {index} deletion value"),
            }
        }
    }

    #[test]
    fn competitor_and_owner_retries_alternate_returned_key_closure() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-competitor-owner-retry-returned-key-closure.orna",
                include_str!("../tests/fixtures/table-rekey-competitor-owner-retry-returned-key-closure.orna"),
                "main",
            )
            .expect("checked-in alternating owner retry fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "first source", 10).encode().unwrap()),
                (key(2), row(2, "original destination", 20).encode().unwrap()),
                (key(3), row(3, "second source", 30).encode().unwrap()),
                (key(6), row(6, "move target blocker", 40).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("competitor and owner retries can alternate as each key owner moves");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 44);

        let expected = [
            (2, Some(4), false, Some(row(4, "original destination", 20))),
            (1, Some(2), false, Some(row(2, "first source", 10))),
            (1, None, true, Some(row(1, "replacement", 30))),
            (2, None, false, Some(row(2, "first source", 11))),
            (1, None, false, Some(row(1, "replacement", 31))),
            (2, None, false, Some(row(2, "first source", 12))),
            (1, None, false, None),
            (2, Some(1), false, Some(row(1, "first source", 12))),
            (1, None, false, Some(row(1, "first source", 13))),
            (3, Some(2), false, Some(row(2, "second source", 30))),
            (2, None, false, Some(row(2, "second source", 31))),
            (1, None, false, Some(row(1, "first source", 14))),
            (1, None, false, None),
            (2, Some(1), false, Some(row(1, "second source", 31))),
            (1, None, false, Some(row(1, "second source", 32))),
            (1, None, false, Some(row(1, "second source", 33))),
            (1, Some(2), false, Some(row(2, "second source", 33))),
            (2, None, false, Some(row(2, "second source", 34))),
            (4, Some(1), false, Some(row(1, "original destination", 20))),
            (1, None, false, Some(row(1, "original destination", 21))),
            (1, None, false, Some(row(1, "original destination", 22))),
            (1, Some(5), false, Some(row(5, "original destination", 22))),
            (5, None, false, Some(row(5, "original destination", 23))),
            (2, Some(1), false, Some(row(1, "second source", 34))),
            (1, None, false, Some(row(1, "second source", 35))),
            (1, Some(4), false, Some(row(4, "second source", 35))),
            (4, None, false, Some(row(4, "second source", 36))),
            (4, None, false, Some(row(4, "second source", 37))),
            (6, None, false, Some(row(6, "move target blocker", 41))),
            (6, None, false, None),
            (4, Some(6), false, Some(row(6, "second source", 37))),
            (6, None, false, Some(row(6, "second source", 38))),
            (5, Some(4), false, Some(row(4, "original destination", 23))),
            (4, None, false, Some(row(4, "original destination", 25))),
            (4, None, false, Some(row(4, "original destination", 26))),
            (4, Some(5), false, Some(row(5, "original destination", 26))),
            (5, None, false, Some(row(5, "original destination", 27))),
            (6, Some(4), false, Some(row(4, "second source", 38))),
            (4, None, false, Some(row(4, "second source", 39))),
            (4, None, false, Some(row(4, "second source", 40))),
            (4, Some(6), false, Some(row(6, "second source", 40))),
            (6, None, false, Some(row(6, "second source", 41))),
            (5, Some(4), false, Some(row(4, "original destination", 27))),
            (4, None, false, Some(row(4, "original destination", 28))),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            match expected_row {
                Some(expected_row) => assert_eq!(
                    CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                    expected_row,
                    "mutation {index} row value"
                ),
                None => assert_eq!(mutation.value(), None, "mutation {index} deletion value"),
            }
        }
    }

    #[test]
    fn original_owner_key_retries_after_competitor_closure() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-original-owner-key-alternating-retry-tail.orna",
                include_str!("../tests/fixtures/table-rekey-original-owner-key-alternating-retry-tail.orna"),
                "main",
            )
            .expect("checked-in original owner-key retry fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(1), row(1, "first source", 10).encode().unwrap()),
                (key(2), row(2, "original destination", 20).encode().unwrap()),
                (key(3), row(3, "second source", 30).encode().unwrap()),
                (key(6), row(6, "move target blocker", 40).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("the original owner key can be reclaimed after competitor retries");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 56);

        let expected = [
            (2, Some(4), false, Some(row(4, "original destination", 20))),
            (1, Some(2), false, Some(row(2, "first source", 10))),
            (1, None, true, Some(row(1, "replacement", 30))),
            (2, None, false, Some(row(2, "first source", 11))),
            (1, None, false, Some(row(1, "replacement", 31))),
            (2, None, false, Some(row(2, "first source", 12))),
            (1, None, false, None),
            (2, Some(1), false, Some(row(1, "first source", 12))),
            (1, None, false, Some(row(1, "first source", 13))),
            (3, Some(2), false, Some(row(2, "second source", 30))),
            (2, None, false, Some(row(2, "second source", 31))),
            (1, None, false, Some(row(1, "first source", 14))),
            (1, None, false, None),
            (2, Some(1), false, Some(row(1, "second source", 31))),
            (1, None, false, Some(row(1, "second source", 32))),
            (1, None, false, Some(row(1, "second source", 33))),
            (1, Some(2), false, Some(row(2, "second source", 33))),
            (2, None, false, Some(row(2, "second source", 34))),
            (4, Some(1), false, Some(row(1, "original destination", 20))),
            (1, None, false, Some(row(1, "original destination", 21))),
            (1, None, false, Some(row(1, "original destination", 22))),
            (1, Some(5), false, Some(row(5, "original destination", 22))),
            (5, None, false, Some(row(5, "original destination", 23))),
            (2, Some(1), false, Some(row(1, "second source", 34))),
            (1, None, false, Some(row(1, "second source", 35))),
            (1, Some(4), false, Some(row(4, "second source", 35))),
            (4, None, false, Some(row(4, "second source", 36))),
            (4, None, false, Some(row(4, "second source", 37))),
            (6, None, false, Some(row(6, "move target blocker", 41))),
            (6, None, false, None),
            (4, Some(6), false, Some(row(6, "second source", 37))),
            (6, None, false, Some(row(6, "second source", 38))),
            (5, Some(4), false, Some(row(4, "original destination", 23))),
            (4, None, false, Some(row(4, "original destination", 25))),
            (4, None, false, Some(row(4, "original destination", 26))),
            (4, Some(5), false, Some(row(5, "original destination", 26))),
            (5, None, false, Some(row(5, "original destination", 27))),
            (6, Some(4), false, Some(row(4, "second source", 38))),
            (4, None, false, Some(row(4, "second source", 39))),
            (4, None, false, Some(row(4, "second source", 40))),
            (4, Some(6), false, Some(row(6, "second source", 40))),
            (6, None, false, Some(row(6, "second source", 41))),
            (5, Some(4), false, Some(row(4, "original destination", 27))),
            (4, None, false, Some(row(4, "original destination", 28))),
            (4, Some(2), false, Some(row(2, "original destination", 28))),
            (2, None, false, Some(row(2, "original destination", 29))),
            (2, None, false, Some(row(2, "original destination", 30))),
            (2, Some(5), false, Some(row(5, "original destination", 30))),
            (5, None, false, Some(row(5, "original destination", 31))),
            (6, Some(2), false, Some(row(2, "second source", 41))),
            (2, None, false, Some(row(2, "second source", 42))),
            (2, None, false, Some(row(2, "second source", 43))),
            (2, Some(6), false, Some(row(6, "second source", 43))),
            (6, None, false, Some(row(6, "second source", 44))),
            (5, Some(2), false, Some(row(2, "original destination", 31))),
            (2, None, false, Some(row(2, "original destination", 32))),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            match expected_row {
                Some(expected_row) => assert_eq!(
                    CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                    expected_row,
                    "mutation {index} row value"
                ),
                None => assert_eq!(mutation.value(), None, "mutation {index} deletion value"),
            }
        }
    }

    #[test]
    fn original_owner_key_and_competitor_alternate_final_retry() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-original-owner-key-returned-competitor-cycle.orna",
                include_str!("../tests/fixtures/table-rekey-original-owner-key-returned-competitor-cycle.orna"),
                "main",
            )
            .expect("checked-in original owner-key closure fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(2), row(2, "original destination", 32).encode().unwrap()),
                (key(6), row(6, "second source", 44).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("owner and competitor retries alternate against the returned key");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 10);

        let expected = [
            (2, None, false, Some(row(2, "original destination", 33))),
            (2, Some(5), false, Some(row(5, "original destination", 33))),
            (5, None, false, Some(row(5, "original destination", 34))),
            (6, Some(2), false, Some(row(2, "second source", 44))),
            (2, None, false, Some(row(2, "second source", 45))),
            (2, None, false, Some(row(2, "second source", 46))),
            (2, Some(6), false, Some(row(6, "second source", 46))),
            (6, None, false, Some(row(6, "second source", 47))),
            (5, Some(2), false, Some(row(2, "original destination", 34))),
            (2, None, false, Some(row(2, "original destination", 35))),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            match expected_row {
                Some(expected_row) => assert_eq!(
                    CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                    expected_row,
                    "mutation {index} row value"
                ),
                None => assert_eq!(mutation.value(), None, "mutation {index} deletion value"),
            }
        }
    }

    #[test]
    fn original_owner_key_retries_after_competitor_deletion() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-original-owner-key-competitor-delete-final-retry.orna",
                include_str!("../tests/fixtures/table-rekey-original-owner-key-competitor-delete-final-retry.orna"),
                "main",
            )
            .expect("checked-in original owner-key deletion fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(2), row(2, "original destination", 32).encode().unwrap()),
                (key(6), row(6, "second source", 44).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("owner retry succeeds after deleting the competitor at its original key");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 9);

        let expected = [
            (2, None, false, Some(row(2, "original destination", 33))),
            (2, Some(5), false, Some(row(5, "original destination", 33))),
            (5, None, false, Some(row(5, "original destination", 34))),
            (6, Some(2), false, Some(row(2, "second source", 44))),
            (2, None, false, Some(row(2, "second source", 45))),
            (2, None, false, Some(row(2, "second source", 46))),
            (2, None, false, None),
            (5, Some(2), false, Some(row(2, "original destination", 34))),
            (2, None, false, Some(row(2, "original destination", 35))),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            match expected_row {
                Some(expected_row) => assert_eq!(
                    CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                    expected_row,
                    "mutation {index} row value"
                ),
                None => assert_eq!(mutation.value(), None, "mutation {index} deletion value"),
            }
        }
    }

    #[test]
    fn original_owner_retries_after_competitor_nested_closure() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-competitor-closure-final-owner-retry.orna",
                include_str!("../tests/fixtures/table-rekey-competitor-closure-final-owner-retry.orna"),
                "main",
            )
            .expect("checked-in competitor closure fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(2), row(2, "original destination", 32).encode().unwrap()),
                (key(6), row(6, "second source", 44).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("original owner retry succeeds after nested competitor closure");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 14);

        let expected = [
            (2, None, false, Some(row(2, "original destination", 33))),
            (2, Some(5), false, Some(row(5, "original destination", 33))),
            (5, None, false, Some(row(5, "original destination", 34))),
            (6, Some(2), false, Some(row(2, "second source", 44))),
            (2, None, false, Some(row(2, "second source", 45))),
            (5, Some(6), false, Some(row(6, "original destination", 34))),
            (6, None, false, Some(row(6, "original destination", 35))),
            (6, None, false, Some(row(6, "original destination", 36))),
            (6, Some(4), false, Some(row(4, "original destination", 36))),
            (4, None, false, Some(row(4, "original destination", 37))),
            (2, Some(6), false, Some(row(6, "second source", 45))),
            (6, None, false, Some(row(6, "second source", 46))),
            (4, Some(2), false, Some(row(2, "original destination", 37))),
            (2, None, false, Some(row(2, "original destination", 38))),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            match expected_row {
                Some(expected_row) => assert_eq!(
                    CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                    expected_row,
                    "mutation {index} row value"
                ),
                None => assert_eq!(mutation.value(), None, "mutation {index} deletion value"),
            }
        }
    }

    #[test]
    fn owner_retries_after_competitor_target_delete_closure() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-owner-retry-competitor-target-delete-closure.orna",
                include_str!("../tests/fixtures/table-rekey-owner-retry-competitor-target-delete-closure.orna"),
                "main",
            )
            .expect("checked-in competitor target closure fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(2), row(2, "original destination", 32).encode().unwrap()),
                (key(4), row(4, "move target blocker", 50).encode().unwrap()),
                (key(6), row(6, "second source", 44).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("the original owner retries after the competitor closes its blocked move");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 11);

        let expected = [
            (2, None, false, Some(row(2, "original destination", 33))),
            (2, Some(5), false, Some(row(5, "original destination", 33))),
            (5, None, false, Some(row(5, "original destination", 34))),
            (6, Some(2), false, Some(row(2, "second source", 44))),
            (2, None, false, Some(row(2, "second source", 45))),
            (4, None, false, Some(row(4, "move target blocker", 55))),
            (4, None, false, None),
            (2, Some(4), false, Some(row(4, "second source", 45))),
            (4, None, false, Some(row(4, "second source", 46))),
            (5, Some(2), false, Some(row(2, "original destination", 34))),
            (2, None, false, Some(row(2, "original destination", 35))),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            match expected_row {
                Some(expected_row) => assert_eq!(
                    CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                    expected_row,
                    "mutation {index} row value"
                ),
                None => assert_eq!(mutation.value(), None, "mutation {index} deletion value"),
            }
        }
    }

    #[test]
    fn owner_retries_after_competitor_target_rekey_closure() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-owner-retry-competitor-target-rekey-closure.orna",
                include_str!("../tests/fixtures/table-rekey-owner-retry-competitor-target-rekey-closure.orna"),
                "main",
            )
            .expect("checked-in competitor target re-key fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(2), row(2, "original destination", 32).encode().unwrap()),
                (key(4), row(4, "move target blocker", 50).encode().unwrap()),
                (key(6), row(6, "second source", 44).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("the original owner retries after the competitor closes its blocked move");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 12);

        let expected = [
            (2, None, false, Some(row(2, "original destination", 33))),
            (2, Some(5), false, Some(row(5, "original destination", 33))),
            (5, None, false, Some(row(5, "original destination", 34))),
            (6, Some(2), false, Some(row(2, "second source", 44))),
            (2, None, false, Some(row(2, "second source", 45))),
            (4, None, false, Some(row(4, "move target blocker", 55))),
            (4, Some(3), false, Some(row(3, "move target blocker", 55))),
            (3, None, false, Some(row(3, "move target blocker", 56))),
            (2, Some(4), false, Some(row(4, "second source", 45))),
            (4, None, false, Some(row(4, "second source", 46))),
            (5, Some(2), false, Some(row(2, "original destination", 34))),
            (2, None, false, Some(row(2, "original destination", 35))),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            match expected_row {
                Some(expected_row) => assert_eq!(
                    CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                    expected_row,
                    "mutation {index} row value"
                ),
                None => assert_eq!(mutation.value(), None, "mutation {index} deletion value"),
            }
        }
    }

    #[test]
    fn owner_retries_after_competitor_closure_reuses_released_target() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-owner-retry-reused-target-closure.orna",
                include_str!("../tests/fixtures/table-rekey-owner-retry-reused-target-closure.orna"),
                "main",
            )
            .expect("checked-in reused target closure fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(2), row(2, "original destination", 32).encode().unwrap()),
                (key(4), row(4, "move target blocker", 50).encode().unwrap()),
                (key(6), row(6, "second source", 44).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("owner retry succeeds after the competitor closes its reused target");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 17);

        let expected = [
            (2, None, false, Some(row(2, "original destination", 33))),
            (2, Some(5), false, Some(row(5, "original destination", 33))),
            (5, None, false, Some(row(5, "original destination", 34))),
            (6, Some(2), false, Some(row(2, "second source", 44))),
            (2, None, false, Some(row(2, "second source", 45))),
            (4, None, false, Some(row(4, "move target blocker", 55))),
            (4, Some(3), false, Some(row(3, "move target blocker", 55))),
            (3, None, false, Some(row(3, "move target blocker", 56))),
            (5, Some(4), false, Some(row(4, "original destination", 34))),
            (4, None, false, Some(row(4, "original destination", 35))),
            (4, None, false, Some(row(4, "original destination", 36))),
            (4, Some(5), false, Some(row(5, "original destination", 36))),
            (5, None, false, Some(row(5, "original destination", 37))),
            (2, Some(4), false, Some(row(4, "second source", 45))),
            (4, None, false, Some(row(4, "second source", 46))),
            (5, Some(2), false, Some(row(2, "original destination", 37))),
            (2, None, false, Some(row(2, "original destination", 38))),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            match expected_row {
                Some(expected_row) => assert_eq!(
                    CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                    expected_row,
                    "mutation {index} row value"
                ),
                None => assert_eq!(mutation.value(), None, "mutation {index} deletion value"),
            }
        }
    }

    #[test]
    fn owner_retries_after_nested_competitor_target_closure() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-owner-retry-nested-competitor-target-closure.orna",
                include_str!("../tests/fixtures/table-rekey-owner-retry-nested-competitor-target-closure.orna"),
                "main",
            )
            .expect("checked-in nested competitor closure fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(2), row(2, "original destination", 32).encode().unwrap()),
                (key(3), row(3, "secondary target blocker", 60).encode().unwrap()),
                (key(4), row(4, "move target blocker", 50).encode().unwrap()),
                (key(6), row(6, "second source", 44).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("owner retry succeeds after the nested competitor closure releases its target");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 15);

        let expected = [
            (2, None, false, Some(row(2, "original destination", 33))),
            (2, Some(5), false, Some(row(5, "original destination", 33))),
            (5, None, false, Some(row(5, "original destination", 34))),
            (6, Some(2), false, Some(row(2, "second source", 44))),
            (2, None, false, Some(row(2, "second source", 45))),
            (4, None, false, Some(row(4, "move target blocker", 55))),
            (3, None, false, Some(row(3, "secondary target blocker", 65))),
            (3, Some(7), false, Some(row(7, "secondary target blocker", 65))),
            (7, None, false, Some(row(7, "secondary target blocker", 66))),
            (4, Some(3), false, Some(row(3, "move target blocker", 55))),
            (3, None, false, Some(row(3, "move target blocker", 56))),
            (2, Some(4), false, Some(row(4, "second source", 45))),
            (4, None, false, Some(row(4, "second source", 46))),
            (5, Some(2), false, Some(row(2, "original destination", 34))),
            (2, None, false, Some(row(2, "original destination", 35))),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            match expected_row {
                Some(expected_row) => assert_eq!(
                    CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                    expected_row,
                    "mutation {index} row value"
                ),
                None => assert_eq!(mutation.value(), None, "mutation {index} deletion value"),
            }
        }
    }

    #[test]
    fn original_owner_participates_in_nested_competitor_closure() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-owner-retry-owner-in-competitor-closure.orna",
                include_str!("../tests/fixtures/table-rekey-owner-retry-owner-in-competitor-closure.orna"),
                "main",
            )
            .expect("checked-in owner and competitor closure fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(2), row(2, "original destination", 32).encode().unwrap()),
                (key(3), row(3, "secondary target blocker", 60).encode().unwrap()),
                (key(4), row(4, "move target blocker", 50).encode().unwrap()),
                (key(6), row(6, "second source", 44).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("owner returns after moving aside inside the nested competitor closure");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 20);

        let expected = [
            (2, None, false, Some(row(2, "original destination", 33))),
            (2, Some(5), false, Some(row(5, "original destination", 33))),
            (5, None, false, Some(row(5, "original destination", 34))),
            (6, Some(2), false, Some(row(2, "second source", 44))),
            (2, None, false, Some(row(2, "second source", 45))),
            (4, None, false, Some(row(4, "move target blocker", 55))),
            (3, None, false, Some(row(3, "secondary target blocker", 65))),
            (5, Some(7), false, Some(row(7, "original destination", 34))),
            (7, None, false, Some(row(7, "original destination", 35))),
            (7, None, false, Some(row(7, "original destination", 36))),
            (7, Some(8), false, Some(row(8, "original destination", 36))),
            (8, None, false, Some(row(8, "original destination", 37))),
            (3, Some(7), false, Some(row(7, "secondary target blocker", 65))),
            (7, None, false, Some(row(7, "secondary target blocker", 66))),
            (4, Some(3), false, Some(row(3, "move target blocker", 55))),
            (3, None, false, Some(row(3, "move target blocker", 56))),
            (2, Some(4), false, Some(row(4, "second source", 45))),
            (4, None, false, Some(row(4, "second source", 46))),
            (8, Some(2), false, Some(row(2, "original destination", 37))),
            (2, None, false, Some(row(2, "original destination", 38))),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            match expected_row {
                Some(expected_row) => assert_eq!(
                    CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                    expected_row,
                    "mutation {index} row value"
                ),
                None => assert_eq!(mutation.value(), None, "mutation {index} deletion value"),
            }
        }
    }

    #[test]
    fn owner_blocks_primary_competitor_closure_retry_then_returns() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-owner-blocks-primary-closure-retry.orna",
                include_str!("../tests/fixtures/table-rekey-owner-blocks-primary-closure-retry.orna"),
                "main",
            )
            .expect("checked-in owner-blocked closure fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(2), row(2, "original destination", 32).encode().unwrap()),
                (key(3), row(3, "secondary target blocker", 60).encode().unwrap()),
                (key(4), row(4, "move target blocker", 50).encode().unwrap()),
                (key(6), row(6, "second source", 44).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("owner returns after blocking and then releasing the competitor closure retry");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 20);

        let expected = [
            (2, None, false, Some(row(2, "original destination", 33))),
            (2, Some(5), false, Some(row(5, "original destination", 33))),
            (5, None, false, Some(row(5, "original destination", 34))),
            (6, Some(2), false, Some(row(2, "second source", 44))),
            (2, None, false, Some(row(2, "second source", 45))),
            (4, None, false, Some(row(4, "move target blocker", 55))),
            (3, None, false, Some(row(3, "secondary target blocker", 65))),
            (3, Some(7), false, Some(row(7, "secondary target blocker", 65))),
            (7, None, false, Some(row(7, "secondary target blocker", 66))),
            (5, Some(3), false, Some(row(3, "original destination", 34))),
            (3, None, false, Some(row(3, "original destination", 35))),
            (3, None, false, Some(row(3, "original destination", 36))),
            (3, Some(8), false, Some(row(8, "original destination", 36))),
            (8, None, false, Some(row(8, "original destination", 37))),
            (4, Some(3), false, Some(row(3, "move target blocker", 55))),
            (3, None, false, Some(row(3, "move target blocker", 56))),
            (2, Some(4), false, Some(row(4, "second source", 45))),
            (4, None, false, Some(row(4, "second source", 46))),
            (8, Some(2), false, Some(row(2, "original destination", 37))),
            (2, None, false, Some(row(2, "original destination", 38))),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            match expected_row {
                Some(expected_row) => assert_eq!(
                    CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                    expected_row,
                    "mutation {index} row value"
                ),
                None => assert_eq!(mutation.value(), None, "mutation {index} deletion value"),
            }
        }
    }

    #[test]
    fn owner_unblocks_its_nested_move_before_competitor_closure() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-owner-move-target-closure-inside-competitor.orna",
                include_str!("../tests/fixtures/table-rekey-owner-move-target-closure-inside-competitor.orna"),
                "main",
            )
            .expect("checked-in owner move closure fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(2), row(2, "original destination", 32).encode().unwrap()),
                (key(3), row(3, "secondary target blocker", 60).encode().unwrap()),
                (key(4), row(4, "move target blocker", 50).encode().unwrap()),
                (key(6), row(6, "second source", 44).encode().unwrap()),
                (key(8), row(8, "owner move blocker", 70).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("owner clears its blocked move before the competitor and owner retry");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 22);

        let expected = [
            (2, None, false, Some(row(2, "original destination", 33))),
            (2, Some(5), false, Some(row(5, "original destination", 33))),
            (5, None, false, Some(row(5, "original destination", 34))),
            (6, Some(2), false, Some(row(2, "second source", 44))),
            (2, None, false, Some(row(2, "second source", 45))),
            (4, None, false, Some(row(4, "move target blocker", 55))),
            (3, None, false, Some(row(3, "secondary target blocker", 65))),
            (3, Some(7), false, Some(row(7, "secondary target blocker", 65))),
            (7, None, false, Some(row(7, "secondary target blocker", 66))),
            (5, Some(3), false, Some(row(3, "original destination", 34))),
            (3, None, false, Some(row(3, "original destination", 35))),
            (8, None, false, Some(row(8, "owner move blocker", 75))),
            (8, Some(9), false, Some(row(9, "owner move blocker", 75))),
            (9, None, false, Some(row(9, "owner move blocker", 76))),
            (3, Some(8), false, Some(row(8, "original destination", 35))),
            (8, None, false, Some(row(8, "original destination", 36))),
            (4, Some(3), false, Some(row(3, "move target blocker", 55))),
            (3, None, false, Some(row(3, "move target blocker", 56))),
            (2, Some(4), false, Some(row(4, "second source", 45))),
            (4, None, false, Some(row(4, "second source", 46))),
            (8, Some(2), false, Some(row(2, "original destination", 36))),
            (2, None, false, Some(row(2, "original destination", 38))),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            match expected_row {
                Some(expected_row) => assert_eq!(
                    CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                    expected_row,
                    "mutation {index} row value"
                ),
                None => assert_eq!(mutation.value(), None, "mutation {index} deletion value"),
            }
        }
    }

    #[test]
    fn owner_move_closure_deletes_blocker_in_competitor_tail() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-owner-move-target-delete-closure-inside-competitor.orna",
                include_str!("../tests/fixtures/table-rekey-owner-move-target-delete-closure-inside-competitor.orna"),
                "main",
            )
            .expect("checked-in owner move deletion closure fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(2), row(2, "original destination", 32).encode().unwrap()),
                (key(3), row(3, "secondary target blocker", 60).encode().unwrap()),
                (key(4), row(4, "move target blocker", 50).encode().unwrap()),
                (key(6), row(6, "second source", 44).encode().unwrap()),
                (key(8), row(8, "owner move blocker", 70).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("owner move closure deletes its target blocker before both retries finish");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 21);

        let expected = [
            (2, None, false, Some(row(2, "original destination", 33))),
            (2, Some(5), false, Some(row(5, "original destination", 33))),
            (5, None, false, Some(row(5, "original destination", 34))),
            (6, Some(2), false, Some(row(2, "second source", 44))),
            (2, None, false, Some(row(2, "second source", 45))),
            (4, None, false, Some(row(4, "move target blocker", 55))),
            (3, None, false, Some(row(3, "secondary target blocker", 65))),
            (3, Some(7), false, Some(row(7, "secondary target blocker", 65))),
            (7, None, false, Some(row(7, "secondary target blocker", 66))),
            (5, Some(3), false, Some(row(3, "original destination", 34))),
            (3, None, false, Some(row(3, "original destination", 35))),
            (8, None, false, Some(row(8, "owner move blocker", 75))),
            (8, None, false, None),
            (3, Some(8), false, Some(row(8, "original destination", 35))),
            (8, None, false, Some(row(8, "original destination", 36))),
            (4, Some(3), false, Some(row(3, "move target blocker", 55))),
            (3, None, false, Some(row(3, "move target blocker", 56))),
            (2, Some(4), false, Some(row(4, "second source", 45))),
            (4, None, false, Some(row(4, "second source", 46))),
            (8, Some(2), false, Some(row(2, "original destination", 36))),
            (2, None, false, Some(row(2, "original destination", 38))),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            match expected_row {
                Some(expected_row) => assert_eq!(
                    CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                    expected_row,
                    "mutation {index} row value"
                ),
                None => assert_eq!(mutation.value(), None, "mutation {index} deletion value"),
            }
        }
    }

    #[test]
    fn owner_move_delete_release_target_reuse_needs_nested_retry() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-rekey-owner-delete-release-target-reuse.orna",
                include_str!("../tests/fixtures/table-rekey-owner-delete-release-target-reuse.orna"),
                "main",
            )
            .expect("checked-in owner target reuse fixture should be admitted");

        let int = |value: i64| {
            CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
        };
        let row = |id: i64, text: &str, quantity: i64| {
            CanonicalValue::new(OvbRaw::Map(vec![
                (OvbRaw::Text("id".into()), OvbRaw::Int(id.into())),
                (OvbRaw::Text("text".into()), OvbRaw::Text(text.into())),
                (OvbRaw::Text("quantity".into()), OvbRaw::Int(quantity.into())),
            ]))
            .expect("row is canonical")
        };
        let key = |value: i64| int(value).encode().expect("key is canonical");
        let rows = BTreeMap::from([(
            "Note".to_owned(),
            vec![
                (key(2), row(2, "original destination", 32).encode().unwrap()),
                (key(3), row(3, "secondary target blocker", 60).encode().unwrap()),
                (key(4), row(4, "move target blocker", 50).encode().unwrap()),
                (key(6), row(6, "second source", 44).encode().unwrap()),
                (key(8), row(8, "owner move blocker", 70).encode().unwrap()),
            ],
        )]);
        let tables = admitted_table_schemas(&application.module_header);
        let mut handler =
            SourceMutationEffectHandler::with_table_rows(tables, rows).expect("valid snapshot");

        invoke_named_with_effects(
            &application.entry,
            &application.functions,
            &Environment::new(),
            application.limits,
            &mut handler,
        )
        .expect("owner retries after the deleted target is reused and released again");

        let mutations = handler.into_mutations().expect("valid ordered mutation log");
        assert_eq!(mutations.len(), 26);

        let expected = [
            (2, None, false, Some(row(2, "original destination", 33))),
            (2, Some(5), false, Some(row(5, "original destination", 33))),
            (5, None, false, Some(row(5, "original destination", 34))),
            (6, Some(2), false, Some(row(2, "second source", 44))),
            (2, None, false, Some(row(2, "second source", 45))),
            (4, None, false, Some(row(4, "move target blocker", 55))),
            (3, None, false, Some(row(3, "secondary target blocker", 65))),
            (3, Some(7), false, Some(row(7, "secondary target blocker", 65))),
            (7, None, false, Some(row(7, "secondary target blocker", 66))),
            (5, Some(3), false, Some(row(3, "original destination", 34))),
            (3, None, false, Some(row(3, "original destination", 35))),
            (8, None, false, Some(row(8, "owner move blocker", 75))),
            (8, None, false, None),
            (4, Some(8), false, Some(row(8, "move target blocker", 55))),
            (8, None, false, Some(row(8, "move target blocker", 57))),
            (8, None, false, Some(row(8, "move target blocker", 58))),
            (8, Some(9), false, Some(row(9, "move target blocker", 58))),
            (9, None, false, Some(row(9, "move target blocker", 59))),
            (3, Some(8), false, Some(row(8, "original destination", 35))),
            (8, None, false, Some(row(8, "original destination", 36))),
            (9, Some(3), false, Some(row(3, "move target blocker", 59))),
            (3, None, false, Some(row(3, "move target blocker", 56))),
            (2, Some(4), false, Some(row(4, "second source", 45))),
            (4, None, false, Some(row(4, "second source", 46))),
            (8, Some(2), false, Some(row(2, "original destination", 36))),
            (2, None, false, Some(row(2, "original destination", 38))),
        ];

        for (index, (mutation, (old_key, new_key, is_insert, expected_row))) in
            mutations.iter().zip(expected).enumerate()
        {
            assert_eq!(mutation.key(), key(old_key), "mutation {index} source key");
            let expected_key = new_key.map(key);
            assert_eq!(
                mutation.rekey_to(),
                expected_key.as_deref(),
                "mutation {index} re-key destination"
            );
            assert_eq!(mutation.is_insert(), is_insert, "mutation {index} insert flag");
            match expected_row {
                Some(expected_row) => assert_eq!(
                    CanonicalValue::decode(mutation.value().unwrap()).unwrap(),
                    expected_row,
                    "mutation {index} row value"
                ),
                None => assert_eq!(mutation.value(), None, "mutation {index} deletion value"),
            }
        }
    }

    #[test]
    fn failed_mutation_tail_does_not_return_earlier_effects_for_staging() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-duplicate-insert-tail.orna",
                include_str!("../tests/fixtures/table-duplicate-insert-tail.orna"),
                "main",
            )
            .expect("checked-in duplicate-key fixture should be admitted");

        let result = authority.evaluate_staged(&application, &Environment::new());
        assert!(matches!(result, Err(ApplicationError::EffectRejected(_))));
    }

    #[test]
    fn update_effect_fails_closed_without_activation_rows() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "table-update-rekey.orna",
                include_str!("../tests/fixtures/table-update-rekey.orna"),
                "main",
            )
            .expect("checked-in table mutation fixture should be admitted");
        assert!(matches!(
            authority.evaluate_staged(&application, &Environment::new()),
            Err(ApplicationError::EffectRejected(_))
        ));
    }

    struct SuccessfulSourceEffectDispatcher;

    impl AsyncApplicationEffectDispatcher for SuccessfulSourceEffectDispatcher {
        fn dispatch<'a>(
            &'a self,
            effect: ApplicationEffectRequest,
            _context: &'a RuntimeActivationContext,
        ) -> ApplicationEffectFuture<'a> {
            Box::pin(async move {
                match effect {
                    ApplicationEffectRequest::PauseStream { .. } => {
                        Ok(CanonicalValue::new(OvbRaw::Bool(true)).expect("canonical bool"))
                    }
                    ApplicationEffectRequest::ResumeStream { .. } => {
                        Ok(CanonicalValue::new(OvbRaw::Bool(true)).expect("canonical bool"))
                    }
                    ApplicationEffectRequest::CancelInvocation { .. } => {
                        Err("unexpected cancellation".to_owned())
                    }
                }
            })
        }
    }

    #[test]
    fn checked_in_source_effect_stages_and_commits_with_activation_scoped_id() {
        use futures::executor::block_on;
        use orna_repository_v1::Repository;
        use orna_runtime_v1::{RuntimeIdentity, RuntimeState};

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock is after Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "orna-source-activation-{}-{timestamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("temporary repository directory");
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&root)
            .status()
            .expect("git init starts");
        assert!(status.success(), "git init creates the runtime repository");
        let repository = Repository::discover(&root).expect("temporary Git repository");
        let state = block_on(RuntimeState::open(
            &repository,
            RuntimeIdentity {
                database_id: [41; 16],
                repository_id: [42; 16],
            },
            [43; 32],
        ))
        .expect("runtime state opens");
        let lease = block_on(state.acquire_lease([44; 16])).expect("writer lease acquired");
        let context = block_on(state.begin_activation()).expect("activation context captured");

        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let application = authority
            .admit_module(
                "admin-pause.orna",
                include_str!("../tests/fixtures/admin-pause-stream.orna"),
                "main",
            )
            .expect("checked-in source fixture is admitted");
        let dispatcher = SuccessfulSourceEffectDispatcher;
        let staged = block_on(authority.evaluate_staged_with_async_effects(
            &application,
            &Environment::from([(
                "stream".to_owned(),
                CanonicalValue::new(OvbRaw::Text("fixture-stream".to_owned()))
                    .expect("canonical stream argument"),
            )]),
            &context,
            &dispatcher,
        ))
        .expect("source table write and terminal runtime effect evaluate");
        assert_eq!(staged.value().raw(), &OvbRaw::Bool(true));
        assert_eq!(staged.mutations().len(), 1);

        let source_id = staged.mutations()[0].id();
        let activation = staged
            .stage(&authority, &context)
            .expect("source mutations cross the captured transaction bridge");
        assert_ne!(activation.mutations()[0].id(), source_id);
        let retry = staged
            .stage(&authority, &context)
            .expect("same captured source activation can be staged again");
        assert_eq!(activation.mutations()[0].id(), retry.mutations()[0].id());
        let key = activation.mutations()[0].key().to_vec();

        block_on(state.commit_table_activation(
            lease,
            &context,
            activation.mutations(),
            activation.next_digest(),
            &NoFault,
        ))
        .expect("owner-fenced table activation commits");
        let fresh_context = block_on(state.begin_activation())
            .expect("later activation captures the committed generation");
        let later_activation = staged
            .stage(&authority, &fresh_context)
            .expect("same source write stages in a later activation");
        assert_ne!(
            later_activation.mutations()[0].id(),
            activation.mutations()[0].id(),
            "later activation must receive a distinct durable mutation ID"
        );
        let committed = block_on(state.committed_table_row("Note", &key))
            .expect("committed row can be read")
            .expect("source insert is durable");
        let row = CanonicalValue::decode(&committed).expect("committed row is canonical");
        let OvbRaw::Map(fields) = row.raw() else {
            panic!("committed row is a record");
        };
        assert!(fields.iter().any(|(field, value)| {
            matches!(field, OvbRaw::Text(name) if name == "text")
                && matches!(value, OvbRaw::Text(text) if text == "staged before pause")
        }));

        drop(state);
        std::fs::remove_dir_all(root).expect("temporary runtime repository removed");
    }

    #[test]
    fn pure_evaluation_fails_closed_on_admitted_table_effects() {
        let authority =
            ApplicationAuthority::new(Catalogue::authoritative_core(), Limits::default());
        let admitted = authority
            .admit_module(
                "main.orna",
                note_source("Note.insert({ id: 7, text: \"once\" });"),
                "main",
            )
            .expect("source should be admitted");
        let error = authority
            .evaluate(&admitted, &Environment::new())
            .expect_err("pure evaluation must not lower table effects");
        assert!(matches!(error, ApplicationError::Evaluation(_)));
    }
}
