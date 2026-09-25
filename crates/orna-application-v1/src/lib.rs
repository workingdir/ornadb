//! The authoritative application boundary for Orna 1.0.
//!
//! This crate deliberately keeps orchestration small: syntax parsing and the
//! semantic resolver/type checker remain in their owning crates, while this
//! boundary is the one place an application source becomes an admitted
//! evaluator program. Runtime table work is staged against the exact captured
//! activation context and is never published by this crate.

use orna_evaluator_v1::{
    EffectHandler, Environment, EvaluationError, Functions, Limits, PureFunction, StepBudget,
    invoke_named, invoke_named_with_effects,
};
use orna_foundation_v1::{CanonicalValue, OvbRaw, SafeText};
use orna_live_v1::{
    Error as LiveError, LiveApplication, LiveApplicationWorkLease, LiveEvalResponse,
    LiveEvalTransaction,
};
use orna_protocol_v1::{Envelope, Message, ResultStatus};
use orna_runtime_v1::{
    NoFault, RuntimeActivationContext, RuntimeError, StagedTableActivation, TableMutation,
};
use orna_semantic_v1::{Catalogue, ModuleInput, SymbolKind, TableSchema, analyze_with_catalogue};
use orna_syntax_v1::{Declaration, Expr, parse_module_with_file};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fmt, future::Future, pin::Pin, sync::Arc, time::UNIX_EPOCH};

const DIGEST_DOMAIN: &[u8] = b"ORNA-ACTIVATION-DIGEST\0";
const SOURCE_MUTATION_DOMAIN: &[u8] = b"ORNA-SOURCE-MUTATION\0";

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

        Ok(AdmittedApplication {
            logical_path,
            source_digest: Sha256::digest(source.as_bytes()).into(),
            entry,
            functions,
            limits: self.limits,
            module_header: analysis
                .modules
                .values()
                .next()
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
        Ok(StagedActivation { value, mutations })
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
        authority.stage_mutations(context.clone(), self.mutations.clone())
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
}

impl SourceMutationEffectHandler {
    #[must_use]
    pub fn new(tables: BTreeMap<String, TableSchema>) -> Self {
        Self {
            tables,
            mutations: Vec::new(),
            next_ordinal: 0,
        }
    }

    /// Consumes the handler after validating every recorded mutation.
    pub fn into_mutations(self) -> Result<Vec<TableMutation>, ApplicationError> {
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
        let mutation = TableMutation::new(id, table, key, encoded)
            .map_err(|_| Self::effect_error("ORNA-EVAL-TABLE-ROW"))?;
        self.next_ordinal = self
            .next_ordinal
            .checked_add(1)
            .ok_or_else(|| Self::effect_error("ORNA-EVAL-LIMIT"))?;
        self.mutations.push(mutation);
        Ok(())
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

impl EffectHandler for SourceMutationEffectHandler {
    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        let Expr::Field { base, name, .. } = callee else {
            return Ok(None);
        };
        let Expr::Name { text: table, .. } = base.as_ref() else {
            return Ok(None);
        };
        let schema = self.table(table)?;
        match name.as_str() {
            "insert" | "upsert" => {
                let [row] = arguments else {
                    return Err(Self::effect_error("ORNA-EVAL-TABLE-ARGUMENT"));
                };
                self.row_matches_schema(schema, row)?;
                let key = self.key_from_row(schema, row)?;
                self.record(table, key, Some(row.clone()))?;
                Ok(Some(row.clone()))
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
                self.record(table, key, None)?;
                Ok(Some(CanonicalValue::unit()))
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
}

fn encoded_key(key: &CanonicalValue) -> Result<Vec<u8>, EvaluationError> {
    key.encode()
        .map_err(|_| SourceMutationEffectHandler::effect_error("ORNA-EVAL-TABLE-KEY"))
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

impl From<ApplicationError> for LiveError {
    fn from(_: ApplicationError) -> Self {
        Self::ApplicationRejected
    }
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
}

impl ApplicationLiveAdapter {
    /// Creates an adapter for the deterministic remote-evaluation module.
    #[must_use]
    pub fn new(authority: ApplicationAuthority) -> Self {
        Self {
            authority,
            logical_path: "remote_eval.orna".to_owned(),
            entry: "main".to_owned(),
        }
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
}

impl LiveApplication for ApplicationLiveAdapter {
    fn eval(
        &mut self,
        _session: [u8; 16],
        request: [u8; 16],
        message: &Message,
    ) -> std::result::Result<Envelope, LiveError> {
        let admitted = self.evaluate_eval_message(message)?;
        let value = self.authority.evaluate(&admitted, &Environment::new())?;
        self.success_envelope(request, eval_fingerprint(message)?, value)
    }

    fn eval_with_transaction<'a>(
        &'a mut self,
        _session: [u8; 16],
        request: [u8; 16],
        message: &'a Message,
        context: Option<&'a RuntimeActivationContext>,
        _work: &'a mut LiveApplicationWorkLease,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<LiveEvalResponse, LiveError>> + 'a>> {
        Box::pin(async move {
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

    fn watch(
        &mut self,
        _session: [u8; 16],
        _request: [u8; 16],
        _message: &Message,
    ) -> std::result::Result<Envelope, LiveError> {
        Err(LiveError::UnsupportedOperation)
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
    entry: String,
    functions: Functions,
    limits: Limits,
    module_header: orna_semantic_v1::ModuleHeader,
}

impl AdmittedApplication {
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
        let admitted = authority
            .admit_module("main.orna", "pub fn main(): Int = 41;", "main")
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
            source: "pub fn main(): Int = 41;".to_owned(),
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
