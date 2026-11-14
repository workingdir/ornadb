//! Stream provider seams: sources, handlers, handler results and provider effects.

use crate::error::RuntimeError;
use crate::request::MAX_TERMINAL_OUTCOME_BYTES;
use crate::stream_run::StreamRunControl;
use crate::table_snapshot::{PinnedCommittedRows, RuntimeTableRows};
use crate::table_store::{Mutation, TableMutation, TableObjectId};
use orna_stream::Checkpoint as StreamCheckpoint;
use orna_stream::{DiagnosticClass, DiagnosticCode, SafeDiagnostic, StreamFailurePayload};
use orna_stream_identity::{CheckpointKey, Component, DeliveryIdentity, Position};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::future::{Future, Ready, ready};
use std::pin::Pin;

/// Connector-owned refetch for a protected failed-delivery reference.
///
/// The runtime supplies the opaque reference only to this callback. Returned
/// bytes are accepted only after the runtime verifies the durable digest;
/// provider errors are intentionally collapsed into a secret-free retry
/// diagnostic.
pub type StreamFailurePayloadFuture<'a, E> = Pin<Box<dyn Future<Output = Result<Vec<u8>, E>> + 'a>>;

pub trait StreamFailurePayloadProvider {
    type Error;

    fn refetch<'a>(&'a self, reference: &'a str) -> StreamFailurePayloadFuture<'a, Self::Error>;
}

pub(crate) struct NoStreamFailurePayloadProvider;

impl StreamFailurePayloadProvider for NoStreamFailurePayloadProvider {
    type Error = ();

    fn refetch<'a>(&'a self, _: &'a str) -> StreamFailurePayloadFuture<'a, Self::Error> {
        Box::pin(async { Err(()) })
    }
}

/// A provider result for one scheduler turn. Provider-specific positions stay
/// inside [`DeliveryIdentity`]; this boundary only admits opaque values.
pub enum StreamSourcePoll {
    Item(Box<StreamItem>),
    Waiting,
    Exhausted,
}

pub struct StreamItem {
    pub delivery: DeliveryIdentity,
    pub payload: Vec<u8>,
}

/// Redacted public metadata for a retained failure payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamFailurePayloadMetadata {
    pub plaintext_bytes: Option<u64>,
    pub protected_reference: bool,
    pub redacted: bool,
}

pub struct StreamMutationBatch {
    pub mutations: Vec<Mutation>,
    pub next_digest: [u8; 32],
}

pub struct StreamTableMutationBatch {
    pub mutations: Vec<TableMutation>,
    pub next_digest: [u8; 32],
}

/// Validates one table-local candidate relation selected by the durable
/// stream transaction. Implementations must treat supplied rows as
/// read-only: the runtime is responsible for the durable mutation and
/// checkpoint commit.
pub trait StreamTableCandidateValidator {
    /// Table relations required to validate this delivery.
    fn tables(&self) -> &[String];

    /// Committed identities the candidate relations were admitted under.
    /// Name-only validators keep the default empty map.
    fn table_object_ids(&self) -> BTreeMap<String, TableObjectId> {
        BTreeMap::new()
    }

    /// Committed `HEAD` rows, pinned to the published checkpoint, that the
    /// candidate relations are read beneath. Name-only validators read no
    /// committed base.
    fn committed_rows(&self) -> Option<&PinnedCommittedRows> {
        None
    }

    /// Returns a safe failure diagnostic when the candidate cannot commit.
    fn validate(&mut self, rows: &RuntimeTableRows) -> Result<(), SafeDiagnostic>;

    /// Validates a boundary whose Orna-controlled writes changed exactly
    /// `changed_tables`. Dependency-scoped propositions (module assertions,
    /// ORNA-ASSERT-026) override this to skip when no dependency changed;
    /// every other validator validates the full candidate, as `validate` does.
    fn validate_changed(
        &mut self,
        rows: &RuntimeTableRows,
        changed_tables: &BTreeSet<String>,
    ) -> Result<(), SafeDiagnostic> {
        let _ = changed_tables;
        self.validate(rows)
    }
}

pub struct StreamValidatedTableMutationBatch {
    pub mutations: Vec<TableMutation>,
    pub next_digest: [u8; 32],
    pub validator: Box<dyn StreamTableCandidateValidator>,
}

pub enum StreamHandlerResult {
    Commit(StreamMutationBatch),
    CommitTable(StreamTableMutationBatch),
    CommitValidatedTable(StreamValidatedTableMutationBatch),
    /// The handler staged table writes in this activation and then failed.
    ///
    /// The staged batch is deliberately discarded: failed delivery
    /// activations must retain only the durable failure record, never table
    /// rows, mutation records, a CWD capture, or checkpoint progress.
    FailAfterTable(StreamTableMutationBatch, SafeDiagnostic),
    Fail(SafeDiagnostic),
    Cancelled,
}

/// A provider failure retained against the exact checkpoint that was being
/// polled. It has no delivery identity because no item was admitted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamProviderFailure {
    pub checkpoint: StreamCheckpoint,
    pub attempts: u32,
    pub diagnostic: SafeDiagnostic,
}

pub(crate) fn is_cancellation_diagnostic(diagnostic: SafeDiagnostic) -> bool {
    diagnostic.code == DiagnosticCode::Cancelled
        || diagnostic.class == DiagnosticClass::Cancellation
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StreamSourceKind {
    Finite,
    Unbounded,
}

/// Stable connector capabilities used to interpret source closure and retry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamSourceDescriptor {
    pub kind: StreamSourceKind,
    pub replayable: bool,
}

pub trait StreamSource {
    type NextFuture<'a>: Future<Output = Result<StreamSourcePoll, SafeDiagnostic>> + 'a
    where
        Self: 'a;
    type WaitFuture<'a>: Future<Output = Result<(), SafeDiagnostic>> + 'a
    where
        Self: 'a;

    fn descriptor(&self) -> StreamSourceDescriptor;
    /// Returns the exact durable stream key served by this connector.
    ///
    /// A connector cannot be admitted without declaring the stable source,
    /// partition, and position-format identities represented by its durable
    /// checkpoint. The runner compares this key before it loads a checkpoint
    /// or polls the provider.
    fn checkpoint_key(&self) -> CheckpointKey;
    /// Selects how a failed item can be preserved for administrative skip or
    /// replay. The default fails closed; every connector that can fail a
    /// delivery must explicitly select plaintext or a protected reference.
    fn failure_payload(&self, _item: &StreamItem) -> StreamFailurePayload {
        StreamFailurePayload::Unavailable
    }
    fn next<'a>(&'a mut self, checkpoint: &'a StreamCheckpoint) -> Self::NextFuture<'a>;
    /// Waits until the source can be polled again or the supplied control is
    /// cancelled. Connectors must wake this future for either event.
    fn wait<'a>(&'a mut self, control: &'a dyn StreamRunControl) -> Self::WaitFuture<'a>;
}

/// A finite, replayable source for the built-in list stream contract.
///
/// The source is stateless between polls: the durable checkpoint selects the
/// next item, so reconstructing the connector after a restart resumes from the
/// same opaque position without relying on process-local cursor state.
pub struct ListStreamSource {
    key: CheckpointKey,
    payloads: Vec<Vec<u8>>,
}

impl ListStreamSource {
    pub fn new(key: CheckpointKey, payloads: Vec<Vec<u8>>) -> Self {
        Self { key, payloads }
    }

    fn next_item(&self, checkpoint: &StreamCheckpoint) -> Result<StreamSourcePoll, SafeDiagnostic> {
        if checkpoint.key != self.key {
            return Err(SafeDiagnostic {
                code: DiagnosticCode::DecodeRejected,
                class: DiagnosticClass::Permanent,
            });
        }
        let index = match &checkpoint.committed {
            None => 0,
            Some(position) => {
                let token = position.token.as_str();
                let canonical = token == "0"
                    || (token.as_bytes().first().is_some_and(|first| *first != b'0')
                        && token.bytes().all(|byte| byte.is_ascii_digit()));
                if !canonical {
                    return Err(SafeDiagnostic {
                        code: DiagnosticCode::DecodeRejected,
                        class: DiagnosticClass::Permanent,
                    });
                }
                token.parse::<usize>().map_err(|_| SafeDiagnostic {
                    code: DiagnosticCode::DecodeRejected,
                    class: DiagnosticClass::Permanent,
                })?
            }
        };
        if index > self.payloads.len() {
            return Err(SafeDiagnostic {
                code: DiagnosticCode::DecodeRejected,
                class: DiagnosticClass::Permanent,
            });
        }
        let Some(payload) = self.payloads.get(index) else {
            return Ok(StreamSourcePoll::Exhausted);
        };
        let position = Position {
            token: Component::new(index.to_string()).map_err(|_| SafeDiagnostic {
                code: DiagnosticCode::Internal,
                class: DiagnosticClass::Permanent,
            })?,
        };
        let successor_index = index.checked_add(1).ok_or(SafeDiagnostic {
            code: DiagnosticCode::Internal,
            class: DiagnosticClass::Permanent,
        })?;
        let successor = Position {
            token: Component::new(successor_index.to_string()).map_err(|_| SafeDiagnostic {
                code: DiagnosticCode::Internal,
                class: DiagnosticClass::Permanent,
            })?,
        };
        Ok(StreamSourcePoll::Item(Box::new(StreamItem {
            delivery: DeliveryIdentity {
                consumer: self.key.consumer.clone(),
                source_format: self.key.source_format.clone(),
                source: self.key.source.clone(),
                partition_format: self.key.partition_format.clone(),
                partition: self.key.partition.clone(),
                position_format: self.key.position_format.clone(),
                position,
                successor,
            },
            payload: payload.clone(),
        })))
    }
}

impl StreamSource for ListStreamSource {
    type NextFuture<'a>
        = Ready<Result<StreamSourcePoll, SafeDiagnostic>>
    where
        Self: 'a;
    type WaitFuture<'a>
        = Ready<Result<(), SafeDiagnostic>>
    where
        Self: 'a;

    fn descriptor(&self) -> StreamSourceDescriptor {
        StreamSourceDescriptor {
            kind: StreamSourceKind::Finite,
            replayable: true,
        }
    }

    fn checkpoint_key(&self) -> CheckpointKey {
        self.key.clone()
    }

    fn failure_payload(&self, item: &StreamItem) -> StreamFailurePayload {
        StreamFailurePayload::Plaintext(item.payload.clone())
    }

    fn next<'a>(&'a mut self, checkpoint: &'a StreamCheckpoint) -> Self::NextFuture<'a> {
        ready(self.next_item(checkpoint))
    }

    fn wait<'a>(&'a mut self, _: &'a dyn StreamRunControl) -> Self::WaitFuture<'a> {
        ready(Ok(()))
    }
}

pub trait StreamHandler {
    fn handle(&mut self, item: &StreamItem) -> StreamHandlerResult;

    /// Returns staged external effects only after `handle` has produced a
    /// successful commit result. The runtime executes these intents before it
    /// atomically commits Orna writes and the source checkpoint.
    fn take_provider_effects(&mut self) -> Vec<StreamProviderEffect> {
        Vec::new()
    }

    /// Replayability is a source-delivery promise, not an external effect
    /// guarantee. A handler must opt into `IdempotentByKey` only when its
    /// provider atomically deduplicates calls by the supplied stable key and
    /// retains the resulting receipt across the runtime retry horizon.
    fn provider_effect_contract(&self, _provider: &str) -> ProviderEffectDeliveryContract {
        ProviderEffectDeliveryContract::ReplayableOnly
    }

    /// Applies one staged effect under the provider's durable idempotency
    /// contract. Returned receipt bytes must be non-secret, deterministic for
    /// a stable key, and no larger than `MAX_TERMINAL_OUTCOME_BYTES`.
    fn apply_provider_effect_once<'a>(
        &'a mut self,
        _key: ProviderEffectKey,
        _effect: &'a StreamProviderEffect,
    ) -> ProviderEffectFuture<'a> {
        Box::pin(std::future::ready(Err(ProviderEffectFailure::Unsupported)))
    }
}

/// Delivery capability declared for external provider effects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderEffectDeliveryContract {
    /// The provider can replay source deliveries but has no idempotent effect
    /// operation. Runtime retries must not claim exactly-once effects.
    ReplayableOnly,
    /// The provider durably deduplicates concurrent/repeated effect calls by
    /// the runtime-supplied `ProviderEffectKey`.
    IdempotentByKey,
}

/// Stable key supplied to an idempotent provider for one effect of a delivery.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ProviderEffectKey(pub(crate) [u8; 32]);

impl ProviderEffectKey {
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// A non-secret receipt returned by an idempotent provider.
pub type ProviderEffectFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<u8>, ProviderEffectFailure>> + 'a>>;

/// Safe provider result class. Arbitrary error strings are intentionally not
/// retained in runtime state or exposed through delivery diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderEffectFailure {
    Retryable,
    Permanent,
    /// The provider may have applied the effect but did not confirm it. This
    /// retries only because the declared idempotency contract reuses the key.
    OutcomeUnknown,
    Unsupported,
}

/// One external effect staged by a successful stream handler.
#[derive(Clone, Eq, PartialEq)]
pub struct StreamProviderEffect {
    provider: String,
    operation: String,
    request: Vec<u8>,
}

impl fmt::Debug for StreamProviderEffect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StreamProviderEffect")
            .field("provider", &self.provider)
            .field("operation", &self.operation)
            .field("request_bytes", &self.request.len())
            .field("request", &"<redacted>")
            .finish()
    }
}

impl StreamProviderEffect {
    pub fn new(
        provider: impl Into<String>,
        operation: impl Into<String>,
        request: Vec<u8>,
    ) -> Result<Self, RuntimeError> {
        let provider = provider.into();
        let operation = operation.into();
        if !valid_provider_effect_name(&provider)
            || !valid_provider_effect_name(&operation)
            || request.len() > MAX_TERMINAL_OUTCOME_BYTES
        {
            return Err(RuntimeError::InvalidIdentity);
        }
        Ok(Self {
            provider,
            operation,
            request,
        })
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn operation(&self) -> &str {
        &self.operation
    }

    pub fn request(&self) -> &[u8] {
        &self.request
    }
}

fn valid_provider_effect_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
}

pub(crate) const MAX_STREAM_PROVIDER_EFFECTS: usize = 64;

/// Maximum provider calls made for one effect under one delivery lease.
pub const MAX_STREAM_PROVIDER_EFFECT_ATTEMPTS: u32 = 3;

pub(crate) enum ProviderEffectPreparation {
    Cached,
    Started { attempt: u32 },
    Rejected(ProviderEffectFailure),
}

pub(crate) enum ProviderEffectDispatchError {
    Runtime(RuntimeError),
    Provider(ProviderEffectFailure),
}
