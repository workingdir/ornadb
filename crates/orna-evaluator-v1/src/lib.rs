//! A deliberately small, deterministic Orna 1.0 expression evaluator.
//!
//! The public boundary admits and returns only OVB-1 canonical values. It has
//! no I/O, external mutation, clock, random, module-loading, or host-call
//! capability.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fmt,
    fmt::Write as _,
    sync::Arc,
};

use num_bigint::{BigInt, Sign};
use num_integer::Integer;
use num_traits::{Signed, ToPrimitive, Zero};
use orna_foundation_v1::{CanonicalValue, Diagnostic, DiagnosticSeverity, SafeText};
use orna_semantic_v1::StandardDependencyProfile;
use orna_syntax_v1::{
    AssignmentOperator, AssignmentTarget, ControlKind, Expr, LiteralKind, Parameter, Pattern,
    PatternField, ReplInput, Statement, StringSegment, parse_expression, parse_repl,
};
use orna_value_v1::{
    CANONICAL_NAN_BITS, ErrorValue as CanonicalErrorValue, Raw, domain_digest, float_max,
    float_min, float_ordinary_eq, float_total_cmp,
};
use serde::{
    Deserialize,
    de::{self, MapAccess, Visitor},
};
use serde_json::value::RawValue;
use sha2::{Digest as _, Sha256};
use unicode_normalization::UnicodeNormalization;

#[cfg(feature = "project-repl")]
mod admitted_repl;
mod cancellation;
mod relation;
mod repl;
mod sys_bindings;
mod timezone;
mod unicode_16_case_properties;

#[cfg(feature = "project-repl")]
pub use admitted_repl::{AdmittedReplSession, ReplError};
pub use cancellation::CancellationToken;
use relation::{
    BucketBySpec, BucketPeriod, RelationBucket, RelationBucketError, RelationBucketState,
    RelationLastState, RelationPlan, RelationStage, RelationWindowState,
};
pub use repl::{ReplSession, parse_admitted_repl};
pub use sys_bindings::SysHostBindingRegistry;
pub use timezone::{
    Instant, LocalDateTime, LocalTimeResolution, TIMEZONE_DATASET_VERSION, TimeZone, TimeZoneError,
    ZonedLocalDateTime, resolve_time_zone,
};

/// The verified standard-source bundle used by the bounded local and remote
/// REPL boundaries. `orna-standard` owns the canonical module source; this
/// crate verifies its pinned profile before either boundary admits an import.
/// Returns the reference standard sources supplied to the bounded REPL.
#[must_use]
pub fn reference_standard_sources() -> [(String, String); 66] {
    orna_standard::reference_standard_sources_v1()
}

/// Returns the immutable profile that verifies [`reference_standard_sources`].
#[must_use]
pub fn reference_standard_profile() -> StandardDependencyProfile {
    orna_standard::reference_standard_profile_v1()
}

const DEFAULT_SOURCE_BYTES: usize = 65_536;
const DEFAULT_STEPS: u64 = 10_000;
const DEFAULT_DEPTH: usize = 64;
const DEFAULT_ITEMS: usize = 1_024;
const DEFAULT_STRING_BYTES: usize = 16_384;
const DEFAULT_INTEGER_DIGITS: usize = 1_024;

fn unicode_16_white_space(value: char) -> bool {
    matches!(
        value,
        '\u{0009}'..='\u{000D}'
            | '\u{0020}'
            | '\u{0085}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
    )
}

fn unicode_16_lowercase(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let scalars = value.chars().collect::<Vec<_>>();
    for (index, scalar) in scalars.iter().copied().enumerate() {
        if scalar == '\u{03a3}' && unicode_16_is_final_sigma(&scalars, index) {
            output.push('\u{03c2}');
            continue;
        }
        let mapping = unicode_case_mapping::to_lowercase(scalar);
        if mapping[0] == 0 {
            output.push(scalar);
            continue;
        }
        for codepoint in mapping.into_iter().take_while(|codepoint| *codepoint != 0) {
            output.push(char::from_u32(codepoint).expect("Unicode casing table contains scalars"));
        }
    }
    output
}

fn unicode_16_is_final_sigma(scalars: &[char], index: usize) -> bool {
    let preceding_cased = scalars[..index]
        .iter()
        .rev()
        .copied()
        .find(|scalar| !unicode_16_case_properties::is_case_ignorable(*scalar))
        .is_some_and(unicode_16_case_properties::is_cased);
    let following_cased = scalars[index + 1..]
        .iter()
        .copied()
        .find(|scalar| !unicode_16_case_properties::is_case_ignorable(*scalar))
        .is_some_and(unicode_16_case_properties::is_cased);
    preceding_cased && !following_cased
}

fn unicode_16_uppercase(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for scalar in value.chars() {
        let mapping = unicode_case_mapping::to_uppercase(scalar);
        if mapping[0] == 0 {
            output.push(scalar);
            continue;
        }
        for codepoint in mapping.into_iter().take_while(|codepoint| *codepoint != 0) {
            output.push(char::from_u32(codepoint).expect("Unicode casing table contains scalars"));
        }
    }
    output
}

/// Explicit resource bounds. All zero values reject evaluation immediately.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub max_source_bytes: usize,
    pub max_steps: u64,
    pub max_depth: usize,
    pub max_collection_items: usize,
    pub max_string_bytes: usize,
    pub max_integer_digits: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_source_bytes: DEFAULT_SOURCE_BYTES,
            max_steps: DEFAULT_STEPS,
            max_depth: DEFAULT_DEPTH,
            max_collection_items: DEFAULT_ITEMS,
            max_string_bytes: DEFAULT_STRING_BYTES,
            max_integer_digits: DEFAULT_INTEGER_DIGITS,
        }
    }
}

impl Limits {
    /// Validate configuration and source bytes before a caller parses source.
    pub fn check_source(self, source: &str) -> Result<(), EvaluationError> {
        check_limits(source, self)
    }

    /// Validate configuration and a retained collection's total item count.
    pub fn check_items(self, count: usize) -> Result<(), EvaluationError> {
        validate_limits(self)?;
        if count > self.max_collection_items {
            Err(error("ORNA-EVAL-LIMIT"))
        } else {
            Ok(())
        }
    }

    /// Validate an integer produced at an evaluation boundary.
    pub fn check_integer(self, value: &BigInt) -> Result<(), EvaluationError> {
        validate_limits(self)?;
        if value.to_str_radix(10).len() > self.max_integer_digits {
            Err(error("ORNA-EVAL-LIMIT"))
        } else {
            Ok(())
        }
    }
}

/// The activation-scoped evaluator step budget shared with handled effects.
///
/// Effect handlers may debit host-side work through the budget-aware hook on
/// [`EffectHandler`]. Existing handlers that implement only [`EffectHandler::handle`]
/// retain their previous behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StepBudget {
    remaining: u64,
}

/// One bounded, canonical page of a relation scan. `next` is an exclusive
/// canonical key cursor owned by the source read scope; when `after` is
/// present, the next cursor MUST be lexicographically greater than it. Equal
/// cursor bytes in different [`RelationReadScope`]s identify independent
/// continuations. An absent cursor means exhaustion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelationPage {
    pub rows: Vec<CanonicalValue>,
    pub next: Option<Vec<u8>>,
}

/// Opaque identity for one relation source in an evaluator plan.
///
/// Cloned plans retain this identity across their page reads; independently
/// created sources receive distinct identities, even when their source names
/// are equal. A newly built plan for a later view refresh receives a fresh
/// identity. The identity follows its source through folds and pagination
/// handoffs; cursor bytes are checkpoints, not identities, and may be reused
/// by sibling subscriptions or later refreshes. Effect handlers should bind
/// each read batch's continuation state by both source name and this identity
/// so paired same-name reads cannot resume one another's cursors.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RelationReadScope(u64);

/// A provider-owned cursor bound to one activation's source identity.
/// Checkpoints are opaque to the evaluator and are advanced only after the
/// consumer commits a delivery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamSourceCursor {
    pub source: String,
    pub identity: [u8; 32],
    pub after: Option<Vec<u8>>,
}

/// One source checkpoint to commit atomically with a consumer callback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamCheckpoint {
    pub source: String,
    pub identity: [u8; 32],
    pub checkpoint: Vec<u8>,
}

/// An ordered provider delivery with its event-time and replay checkpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamDelivery {
    pub source: String,
    pub identity: [u8; 32],
    pub checkpoint: Vec<u8>,
    pub event_time: Instant,
    pub value: CanonicalValue,
}

/// A provider or decoding error associated with a blocked ordered delivery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamFailure {
    pub source: String,
    pub identity: [u8; 32],
    pub checkpoint: Option<Vec<u8>>,
    pub event_time: Instant,
    pub error: EvaluationError,
    /// Only errors explicitly marked recoverable by the provider may reach a
    /// `std.stream.recover` handler.
    pub recoverable: bool,
}

/// A source event in provider-observed arrival order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamEvent {
    Delivery(StreamDelivery),
    Failure(StreamFailure),
}

/// One bounded provider poll. The provider supplies events in observed
/// arrival order, preserves each source's order, and advances `watermark`
/// when event-time quiet periods have elapsed. An empty nonterminal page must
/// advance its watermark so consumers never busy-spin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamPage {
    pub events: Vec<StreamEvent>,
    pub watermark: Instant,
    pub ended_sources: BTreeSet<String>,
}

impl StepBudget {
    /// Create an activation budget with the supplied step capacity.
    #[must_use]
    pub fn new(max_steps: u64) -> Self {
        Self {
            remaining: max_steps,
        }
    }

    /// Debit work from this activation's remaining step budget.
    pub fn debit(&mut self, steps: u64) -> Result<(), EvaluationError> {
        if steps > self.remaining {
            self.remaining = 0;
            Err(error("ORNA-EVAL-LIMIT"))
        } else {
            self.remaining -= steps;
            Ok(())
        }
    }

    /// Return the remaining steps available to this activation.
    #[must_use]
    pub fn remaining(&self) -> u64 {
        self.remaining
    }
}

/// A deterministic name environment. Values must be canonical OVB-1 values.
/// Qualified enum-label patterns resolve through the retained enum metadata
/// supplied with the admitted nominal definitions.
pub type Environment = BTreeMap<String, CanonicalValue>;

/// A declaration-owned nominal field admitted to the evaluator.
///
/// The semantic layer remains responsible for checking the field's static
/// type. The evaluator only receives the already-admitted expression and its
/// visibility/default plan, preserving the runtime ownership boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NominalField {
    field_id: Raw,
    name: String,
    public: bool,
    default: Option<Expr>,
}

impl NominalField {
    /// Create a public required field.
    #[must_use]
    pub fn public(field_id: [u8; 16], name: impl Into<String>) -> Self {
        Self {
            field_id: object_id_raw(field_id),
            name: name.into(),
            public: true,
            default: None,
        }
    }

    /// Create a public field with its declaration-owned default expression.
    #[must_use]
    pub fn public_with_default(field_id: [u8; 16], name: impl Into<String>, default: Expr) -> Self {
        Self {
            field_id: object_id_raw(field_id),
            name: name.into(),
            public: true,
            default: Some(default),
        }
    }

    /// Create a private required field.
    #[must_use]
    pub fn private(field_id: [u8; 16], name: impl Into<String>) -> Self {
        Self {
            field_id: object_id_raw(field_id),
            name: name.into(),
            public: false,
            default: None,
        }
    }

    /// Create a private field with its declaration-owned default expression.
    #[must_use]
    pub fn private_with_default(
        field_id: [u8; 16],
        name: impl Into<String>,
        default: Expr,
    ) -> Self {
        Self {
            field_id: object_id_raw(field_id),
            name: name.into(),
            public: false,
            default: Some(default),
        }
    }
}

/// A declaration-owned enum variant retained by the evaluator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NominalVariant {
    variant_id: Raw,
    name: String,
}

impl NominalVariant {
    /// Create an admitted enum variant with its stable ObjectId.
    #[must_use]
    pub fn new(variant_id: [u8; 16], name: impl Into<String>) -> Self {
        Self {
            variant_id: object_id_raw(variant_id),
            name: name.into(),
        }
    }
}

/// A declaration-owned nominal construction plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NominalDefinition {
    /// The canonical OVB identity retained in tag 60009.
    type_id: Raw,
    /// The module namespace which may construct private fields.
    owner: Option<String>,
    /// Declaration order is significant for defaults and output fields.
    fields: Vec<NominalField>,
    /// Closed enum variants retained for qualified constructor patterns.
    variants: Vec<NominalVariant>,
}

impl NominalDefinition {
    /// Create an admitted declaration plan without exposing its private
    /// field names or default expressions through public field access.
    #[must_use]
    pub fn new(type_id: [u8; 16], owner: Option<String>, fields: Vec<NominalField>) -> Self {
        Self {
            type_id: object_id_raw(type_id),
            owner,
            fields,
            variants: Vec::new(),
        }
    }

    /// Retain the closed enum variants for qualified constructor patterns.
    #[must_use]
    pub fn with_enum_variants(mut self, variants: Vec<NominalVariant>) -> Self {
        self.variants = variants;
        self
    }
}

/// Trusted evaluator definitions keyed by admitted source spellings.
pub type NominalDefinitions = BTreeMap<String, NominalDefinition>;

fn object_id_raw(object_id: [u8; 16]) -> Raw {
    Raw::Tag(37, Box::new(Raw::Bytes(object_id.to_vec())))
}

fn is_object_id_raw(value: &Raw) -> bool {
    matches!(
        value,
        Raw::Tag(37, bytes) if matches!(bytes.as_ref(), Raw::Bytes(bytes) if bytes.len() == 16)
    )
}

fn object_id_key(value: &Raw) -> Option<[u8; 16]> {
    let Raw::Tag(37, bytes) = value else {
        return None;
    };
    let Raw::Bytes(bytes) = bytes.as_ref() else {
        return None;
    };
    bytes.as_slice().try_into().ok()
}

fn validate_nominal_definition(
    definition: &NominalDefinition,
    limits: Limits,
) -> Result<BTreeSet<[u8; 16]>, EvaluationError> {
    if !is_object_id_raw(&definition.type_id) {
        return Err(error("ORNA-EVAL-VALUE"));
    }
    if !definition.variants.is_empty() && !definition.fields.is_empty() {
        return Err(error("ORNA-EVAL-VALUE"));
    }
    limits.check_items(definition.fields.len())?;
    limits.check_items(definition.variants.len())?;
    let name_bytes = definition.owner.as_ref().map_or(0, String::len);
    let name_bytes = definition
        .fields
        .iter()
        .try_fold(name_bytes, |total, field| {
            total
                .checked_add(field.name.len())
                .ok_or_else(|| error("ORNA-EVAL-LIMIT"))
        })?;
    let name_bytes = definition
        .variants
        .iter()
        .try_fold(name_bytes, |total, variant| {
            total
                .checked_add(variant.name.len())
                .ok_or_else(|| error("ORNA-EVAL-LIMIT"))
        })?;
    if name_bytes > limits.max_string_bytes {
        return Err(error("ORNA-EVAL-LIMIT"));
    }
    let mut names = BTreeSet::new();
    let mut field_ids = BTreeSet::new();
    for field in &definition.fields {
        let Some(field_id) = object_id_key(&field.field_id) else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        if !names.insert(field.name.clone()) || !field_ids.insert(field_id) {
            return Err(error("ORNA-EVAL-VALUE"));
        }
    }
    let mut variant_names = BTreeSet::new();
    let mut variant_ids = BTreeSet::new();
    for variant in &definition.variants {
        let Some(variant_id) = object_id_key(&variant.variant_id) else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        if !variant_names.insert(variant.name.clone()) || !variant_ids.insert(variant_id) {
            return Err(error("ORNA-EVAL-VALUE"));
        }
    }
    Ok(field_ids)
}

fn validate_nominal_definitions(
    definitions: &NominalDefinitions,
    limits: Limits,
) -> Result<(), EvaluationError> {
    limits.check_items(definitions.len())?;
    let mut total_fields = 0usize;
    let mut total_variants = 0usize;
    let mut total_name_bytes = 0usize;
    for (name, definition) in definitions {
        total_fields = total_fields
            .checked_add(definition.fields.len())
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        total_variants = total_variants
            .checked_add(definition.variants.len())
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        total_name_bytes = total_name_bytes
            .checked_add(name.len())
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        total_name_bytes = total_name_bytes
            .checked_add(definition.owner.as_ref().map_or(0, String::len))
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        total_name_bytes = total_name_bytes
            .checked_add(definition.fields.iter().try_fold(0usize, |total, field| {
                total
                    .checked_add(field.name.len())
                    .ok_or_else(|| error("ORNA-EVAL-LIMIT"))
            })?)
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        total_name_bytes = total_name_bytes
            .checked_add(
                definition
                    .variants
                    .iter()
                    .try_fold(0usize, |total, variant| {
                        total
                            .checked_add(variant.name.len())
                            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))
                    })?,
            )
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
    }
    limits.check_items(total_fields)?;
    limits.check_items(total_variants)?;
    if total_name_bytes > limits.max_string_bytes {
        return Err(error("ORNA-EVAL-LIMIT"));
    }
    let mut type_ids = BTreeSet::new();
    for definition in definitions.values() {
        let Some(type_id) = object_id_key(&definition.type_id) else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        if !type_ids.insert(type_id) {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        validate_nominal_definition(definition, limits)?;
    }
    Ok(())
}

fn validate_admitted_nominals(
    value: &Value,
    definitions: &NominalDefinitions,
    limits: Limits,
) -> Result<(), EvaluationError> {
    match value {
        Value::List(values) | Value::Tuple(values) => {
            for value in values {
                validate_admitted_nominals(value, definitions, limits)?;
            }
        }
        Value::Stream { values, .. } => {
            for value in values {
                validate_admitted_nominals(value, definitions, limits)?;
            }
        }
        Value::Record(fields) => {
            for value in fields.values() {
                validate_admitted_nominals(value, definitions, limits)?;
            }
        }
        Value::NominalRecord { type_id, fields } => {
            let mut definition = None;
            for candidate in definitions.values() {
                if candidate.type_id == *type_id {
                    if definition.is_some() {
                        return Err(error("ORNA-EVAL-VALUE"));
                    }
                    definition = Some(candidate);
                }
            }
            if let Some(definition) = definition {
                let definition_field_ids = validate_nominal_definition(definition, limits)?;
                let mut field_ids = BTreeSet::new();
                for (key, _) in fields {
                    let Some(field_id) = object_id_key(key) else {
                        return Err(error("ORNA-EVAL-VALUE"));
                    };
                    if !field_ids.insert(field_id) {
                        return Err(error("ORNA-EVAL-VALUE"));
                    }
                }
                if field_ids != definition_field_ids {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
            }
            for (_, value) in fields {
                validate_admitted_nominals(value, definitions, limits)?;
            }
        }
        Value::Enum { payload, .. } | Value::Option(payload) => {
            if let Some(value) = payload {
                validate_admitted_nominals(value, definitions, limits)?;
            }
        }
        Value::Range { lower, upper, .. } => {
            if let Some(value) = lower {
                validate_admitted_nominals(value, definitions, limits)?;
            }
            if let Some(value) = upper {
                validate_admitted_nominals(value, definitions, limits)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// An admitted pure function and its lexical immutable value environment.
#[derive(Clone, Debug)]
pub struct PureFunction {
    pub parameters: Vec<Parameter>,
    pub body: Expr,
    pub environment: Environment,
}

/// Explicitly admitted named functions; no host or module lookup is performed.
pub type Functions = BTreeMap<String, PureFunction>;

/// Internal key for an import resolved relative to a captured module body.
/// The separator cannot occur in a source-level Orna name.
pub(crate) fn module_function_alias_key(namespace: &str, name: &str) -> String {
    format!("{namespace}::{name}")
}

/// A payload-free, stable failure suitable for conformance adapters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvaluationError {
    diagnostic: Box<Diagnostic>,
    canonical: Option<CanonicalErrorValue>,
    deliberate: bool,
}

impl EvaluationError {
    /// Constructs a payload-free error from an already-admitted safe code.
    /// Effect handlers cannot attach source, argument, or host payloads.
    pub fn redacted(code: SafeText) -> Self {
        let canonical_code = code.as_str().to_owned();
        Self {
            diagnostic: Box::new(
                Diagnostic::new(code, DiagnosticSeverity::Error, SafeText::redacted())
                    .expect("safe diagnostic code")
                    .redacted(),
            ),
            canonical: Some(
                CanonicalErrorValue::new(canonical_code, "<redacted>", [], BTreeMap::new())
                    .expect("redacted error is canonical"),
            ),
            deliberate: false,
        }
    }

    fn from_canonical(value: CanonicalErrorValue) -> Self {
        Self {
            diagnostic: Box::new(
                Diagnostic::new(
                    SafeText::new("ORNA-EVAL-ERROR").expect("static safe code"),
                    DiagnosticSeverity::Error,
                    SafeText::redacted(),
                )
                .expect("safe diagnostic code")
                .redacted(),
            ),
            canonical: Some(value),
            deliberate: true,
        }
    }

    /// Returns a deliberate canonical Error identity when one is retained.
    pub fn canonical_error(&self) -> Option<&CanonicalErrorValue> {
        self.canonical.as_ref().filter(|_| self.deliberate)
    }

    fn handler_error(&self) -> Result<CanonicalErrorValue, EvaluationError> {
        self.canonical.clone().ok_or_else(|| self.clone())
    }
    pub fn diagnostic(&self) -> &Diagnostic {
        &self.diagnostic
    }
    pub fn code(&self) -> &str {
        self.diagnostic.code()
    }
}
impl fmt::Display for EvaluationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for EvaluationError {}

/// Executes an admitted effect after its arguments have been evaluated exactly
/// once into canonical values. Returning `Ok(None)` leaves the call to the
/// ordinary pure evaluator.
pub trait EffectHandler {
    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError>;

    /// Handle an effect while sharing the activation's step budget.
    ///
    /// The default delegates to [`EffectHandler::handle`] so existing effect
    /// handlers remain source-compatible and retain their prior behavior.
    fn handle_with_budget(
        &mut self,
        callee: &Expr,
        arguments: &[CanonicalValue],
        _budget: &mut StepBudget,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        self.handle(callee, arguments)
    }

    /// Handle a call resolved to its canonical admitted function name. The
    /// default preserves existing handlers; registry-backed bindings can use
    /// the resolved name so source aliases do not bypass native dispatch.
    fn handle_registered_with_budget(
        &mut self,
        _operation: &str,
        callee: &Expr,
        arguments: &[CanonicalValue],
        budget: &mut StepBudget,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        self.handle_with_budget(callee, arguments, budget)
    }

    /// Handle a registered effect while retaining the evaluator's live
    /// cancellation request for host operations that can stop promptly.
    fn handle_registered_with_cancellation_and_budget(
        &mut self,
        operation: &str,
        callee: &Expr,
        arguments: &[CanonicalValue],
        budget: &mut StepBudget,
        _cancellation: Option<&CancellationToken>,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        self.handle_registered_with_budget(operation, callee, arguments, budget)
    }

    /// Forks an activation-owned handler for one structured child task.
    ///
    /// Returning `None` leaves child host effects unavailable. Implementors
    /// that return a handler must give the child an isolated ownership scope;
    /// child adapters are moved to their worker and joined before the parent
    /// activation returns.
    fn fork_task_child(&mut self, _child_index: usize) -> Option<Box<dyn EffectHandler + Send>> {
        None
    }

    /// Resolve a stored row reference at the reference's snapshot pin.
    ///
    /// An activation adapter may apply its private write overlay at that pin,
    /// but must not substitute a different database, table, key, or snapshot.
    /// Returning `None` means the non-optional target could not be resolved.
    fn resolve_reference(
        &mut self,
        _reference: &CanonicalValue,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        Ok(None)
    }

    /// Resolve a row reference while sharing the activation's evaluator budget.
    fn resolve_reference_with_budget(
        &mut self,
        reference: &CanonicalValue,
        _budget: &mut StepBudget,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        self.resolve_reference(reference)
    }

    /// Validate a relation source without enumerating rows. Runtime handlers
    /// use this to preserve relation visibility when a plan such as `take(0)`
    /// can return without requesting its first page.
    fn validate_relation_source(&mut self, _source: &str) -> Result<(), EvaluationError> {
        Ok(())
    }

    /// Supplies one bounded page for an evaluator-owned relation plan at its
    /// first observation. Existing handlers return `Ok(None)` by default so
    /// this remains a backward-compatible extension of the effect boundary.
    fn scan_relation_page(
        &mut self,
        _source: &str,
        _after: Option<&[u8]>,
        _limit: usize,
        _budget: &mut StepBudget,
    ) -> Result<Option<RelationPage>, EvaluationError> {
        Ok(None)
    }

    /// Supplies a bounded page together with the identity of its relation
    /// source. Implementations that retain page continuations should key them
    /// by both `source` and `scope`: same-name planned reads are independent,
    /// and a fresh scope starts a fresh observation during a later refresh.
    /// Repeated cursor bytes do not transfer a continuation between scopes.
    /// The default delegates to [`EffectHandler::scan_relation_page`] to
    /// preserve existing handlers while allowing stateful readers to keep
    /// continuation cursors separate across equal-named view sources.
    fn scan_relation_page_scoped(
        &mut self,
        source: &str,
        _scope: RelationReadScope,
        after: Option<&[u8]>,
        limit: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<RelationPage>, EvaluationError> {
        self.scan_relation_page(source, after, limit, budget)
    }

    /// Validate a provider stream binding before an activation constructs a
    /// stream handle. The source identity is a stable digest of the caller's
    /// identity label and provider source name.
    fn validate_stream_source(
        &mut self,
        _source: &str,
        _identity: &[u8; 32],
        _budget: &mut StepBudget,
        _cancellation: Option<&CancellationToken>,
    ) -> Result<bool, EvaluationError> {
        Ok(false)
    }

    /// Poll at most `max_events` deliveries for the supplied activation-owned
    /// cursors. Implementations return merge events in observed arrival order
    /// and checkpoint values that strictly increase lexicographically within
    /// each source. Polling may wait for data or watermark progress, and must
    /// honor cancellation. The evaluator checks the page bound and cursor
    /// progression before invoking any callback.
    fn poll_stream_sources(
        &mut self,
        _sources: &[StreamSourceCursor],
        _max_events: usize,
        _budget: &mut StepBudget,
        _cancellation: Option<&CancellationToken>,
    ) -> Result<StreamPage, EvaluationError> {
        Err(error("ORNA-EVAL-UNSUPPORTED"))
    }

    /// Begin an activation transaction for one callback and its checkpoint
    /// updates. Host effects issued by the callback use this same handler, so
    /// `commit_stream_delivery` can atomically publish them with the source
    /// checkpoints.
    fn begin_stream_delivery(
        &mut self,
        _checkpoints: &[StreamCheckpoint],
        _budget: &mut StepBudget,
        _cancellation: Option<&CancellationToken>,
    ) -> Result<(), EvaluationError> {
        Err(error("ORNA-EVAL-UNSUPPORTED"))
    }

    /// Commit callback effects and all listed source checkpoints atomically.
    fn commit_stream_delivery(
        &mut self,
        _checkpoints: &[StreamCheckpoint],
        _budget: &mut StepBudget,
        _cancellation: Option<&CancellationToken>,
    ) -> Result<(), EvaluationError> {
        Err(error("ORNA-EVAL-UNSUPPORTED"))
    }

    /// Roll back an uncommitted callback and leave every listed source cursor
    /// unchanged. Implementations should make rollback idempotent.
    fn rollback_stream_delivery(
        &mut self,
        _checkpoints: &[StreamCheckpoint],
        _budget: &mut StepBudget,
        _cancellation: Option<&CancellationToken>,
    ) -> Result<(), EvaluationError> {
        Err(error("ORNA-EVAL-UNSUPPORTED"))
    }
}

/// Evaluate one expression using [`parse_expression`].
pub fn evaluate_expression(
    source: &str,
    environment: &Environment,
    limits: Limits,
) -> Result<CanonicalValue, EvaluationError> {
    evaluate_expression_with_functions(source, environment, &Functions::new(), limits)
}

/// Evaluate source against explicit values and pure functions, checking source
/// limits before parsing. No module or host lookup is performed.
pub fn evaluate_expression_with_functions(
    source: &str,
    environment: &Environment,
    functions: &Functions,
    limits: Limits,
) -> Result<CanonicalValue, EvaluationError> {
    check_limits(source, limits)?;
    let parsed = parse_expression(source);
    if !parsed.is_ok() {
        return Err(error("ORNA-EVAL-PARSE"));
    }
    evaluate_with_functions(&parsed.value, environment, functions, limits)
}

/// Evaluate one expression with an activation-owned effect and stream
/// provider. Provider calls, callback effects, and stream checkpoints share a
/// single evaluator activation.
pub fn evaluate_expression_with_effects(
    source: &str,
    environment: &Environment,
    limits: Limits,
    effects: &mut dyn EffectHandler,
) -> Result<CanonicalValue, EvaluationError> {
    evaluate_expression_with_effects_and_cancellation(source, environment, limits, effects, None)
}

/// Evaluate an expression with effects and an optional live cancellation
/// request. Stream providers receive this same token during polls and
/// checkpoint transactions and must stop promptly when it is requested.
pub fn evaluate_expression_with_effects_and_cancellation(
    source: &str,
    environment: &Environment,
    limits: Limits,
    effects: &mut dyn EffectHandler,
    cancellation: Option<&CancellationToken>,
) -> Result<CanonicalValue, EvaluationError> {
    evaluate_expression_with_functions_and_effects_and_cancellation(
        source,
        environment,
        &Functions::new(),
        limits,
        effects,
        cancellation,
    )
}

/// Evaluate an expression with an explicit pure-function environment and a
/// host effect adapter. This is useful to evaluate source that was already
/// admitted against a module profile while retaining activation-scoped host
/// capabilities.
pub fn evaluate_expression_with_functions_and_effects(
    source: &str,
    environment: &Environment,
    functions: &Functions,
    limits: Limits,
    effects: &mut dyn EffectHandler,
) -> Result<CanonicalValue, EvaluationError> {
    evaluate_expression_with_functions_and_effects_and_cancellation(
        source,
        environment,
        functions,
        limits,
        effects,
        None,
    )
}

/// Evaluate admitted expression source, functions, host effects, and optional
/// activation cancellation through one bounded evaluator context.
pub fn evaluate_expression_with_functions_and_effects_and_cancellation(
    source: &str,
    environment: &Environment,
    functions: &Functions,
    limits: Limits,
    effects: &mut dyn EffectHandler,
    cancellation: Option<&CancellationToken>,
) -> Result<CanonicalValue, EvaluationError> {
    check_limits(source, limits)?;
    let parsed = parse_expression(source);
    if !parsed.is_ok() {
        return Err(error("ORNA-EVAL-PARSE"));
    }
    validate_limits(limits)?;
    let mut context = Context {
        limits,
        steps: 0,
        functions,
        aliases: None,
        session_functions: None,
        repl_bindings: false,
        restrict_function_names: false,
        reject_unhandled_field_calls: false,
        effects: Some(effects),
        namespace: None,
        transfer: None,
        cancellation,
    };
    context.items(functions.len())?;
    let mut scope = Scope::from_environment(environment, &mut context)?;
    let value = context.evaluate(&parsed.value, &mut scope, 0)?;
    if context.transfer.is_some() {
        return Err(error("ORNA-EVAL-UNSUPPORTED"));
    }
    value.canonical()
}

/// Evaluate a REPL expression using [`parse_repl`]. REPL declarations are
/// intentionally outside this evaluator's side-effect-free subset.
pub fn evaluate_repl(
    source: &str,
    environment: &Environment,
    limits: Limits,
) -> Result<CanonicalValue, EvaluationError> {
    check_limits(source, limits)?;
    let parsed = parse_repl(source);
    if !parsed.is_ok() {
        return Err(error("ORNA-EVAL-PARSE"));
    }
    match parsed.value {
        ReplInput::Expression(expression) => evaluate_parsed(&expression, environment, limits),
        ReplInput::Item(_) => Err(error("ORNA-EVAL-UNSUPPORTED")),
    }
}

/// Evaluate an already parsed expression. This is the conformance integration
/// seam; the source API above is merely a parser adapter.
pub fn evaluate_parsed(
    expression: &Expr,
    environment: &Environment,
    limits: Limits,
) -> Result<CanonicalValue, EvaluationError> {
    evaluate_with_functions(expression, environment, &Functions::new(), limits)
}

/// Evaluate a parsed expression with declaration-owned nominal construction
/// plans. Definitions must have already passed semantic admission; this
/// boundary does not infer fields, types, or visibility from source text.
pub fn evaluate_parsed_with_nominals(
    expression: &Expr,
    environment: &Environment,
    nominal_definitions: &NominalDefinitions,
    limits: Limits,
) -> Result<CanonicalValue, EvaluationError> {
    evaluate_with_functions_and_nominals(
        expression,
        environment,
        &Functions::new(),
        nominal_definitions,
        limits,
    )
}

/// Evaluate a parsed expression with an explicit pure-function namespace.
/// Nested calls share the same limits and cannot access the caller's locals.
pub fn evaluate_with_functions(
    expression: &Expr,
    environment: &Environment,
    functions: &Functions,
    limits: Limits,
) -> Result<CanonicalValue, EvaluationError> {
    evaluate_with_functions_and_nominals(
        expression,
        environment,
        functions,
        &NominalDefinitions::new(),
        limits,
    )
}

/// Evaluate a parsed expression with explicit functions and admitted nominal
/// construction plans.
pub fn evaluate_with_functions_and_nominals(
    expression: &Expr,
    environment: &Environment,
    functions: &Functions,
    nominal_definitions: &NominalDefinitions,
    limits: Limits,
) -> Result<CanonicalValue, EvaluationError> {
    let mut budget = StepBudget::new(limits.max_steps);
    evaluate_with_functions_and_nominals_and_budget(
        expression,
        environment,
        functions,
        nominal_definitions,
        limits,
        &mut budget,
    )
}

/// Evaluate a parsed expression while consuming a caller-owned activation
/// budget. The budget includes expression evaluation and any work debited by
/// handled effects, so callers can continue accounting across host boundaries.
pub fn evaluate_with_functions_and_budget(
    expression: &Expr,
    environment: &Environment,
    functions: &Functions,
    limits: Limits,
    budget: &mut StepBudget,
) -> Result<CanonicalValue, EvaluationError> {
    evaluate_with_functions_and_nominals_and_budget(
        expression,
        environment,
        functions,
        &NominalDefinitions::new(),
        limits,
        budget,
    )
}

fn evaluate_with_functions_and_nominals_and_budget(
    expression: &Expr,
    environment: &Environment,
    functions: &Functions,
    nominal_definitions: &NominalDefinitions,
    limits: Limits,
    budget: &mut StepBudget,
) -> Result<CanonicalValue, EvaluationError> {
    validate_limits(limits)?;
    let mut context = Context {
        limits,
        steps: limits.max_steps.saturating_sub(budget.remaining()),
        functions,
        aliases: None,
        session_functions: None,
        repl_bindings: false,
        restrict_function_names: false,
        reject_unhandled_field_calls: false,
        effects: None,
        namespace: None,
        transfer: None,
        cancellation: None,
    };
    let result = (|| {
        context.items(functions.len())?;
        let mut scope =
            Scope::from_environment_with_nominals(environment, nominal_definitions, &mut context)?;
        let value = context.evaluate(expression, &mut scope, 0)?;
        if context.transfer.is_some() {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        value.canonical()
    })();
    *budget = StepBudget::new(limits.max_steps.saturating_sub(context.steps));
    result
}

/// Invoke a parsed, statically checked pure function with canonical named
/// arguments. Defaults execute in declaration order after earlier parameters
/// are bound, and only when omitted. Argument admission, defaults, and the body
/// share one resource budget. This does not provide module or external calls.
pub fn evaluate_function(
    parameters: &[Parameter],
    body: &Expr,
    environment: &Environment,
    arguments: &Environment,
    limits: Limits,
) -> Result<CanonicalValue, EvaluationError> {
    validate_limits(limits)?;
    let functions = Functions::new();
    let mut context = Context {
        limits,
        steps: 0,
        functions: &functions,
        aliases: None,
        session_functions: None,
        repl_bindings: false,
        restrict_function_names: false,
        reject_unhandled_field_calls: false,
        effects: None,
        namespace: None,
        transfer: None,
        cancellation: None,
    };
    let supplied = supplied_arguments(arguments, &NominalDefinitions::new(), &mut context)?;
    let captured = Scope::from_environment(environment, &mut context)?;
    invoke_pure(&mut context, parameters, body, captured, supplied, 0).and_then(Value::canonical)
}

/// Invoke an admitted named function with canonical host-provided arguments.
/// Its body and defaults can call other admitted functions within one budget.
pub fn invoke_named(
    name: &str,
    functions: &Functions,
    arguments: &Environment,
    limits: Limits,
) -> Result<CanonicalValue, EvaluationError> {
    invoke_named_with_nominals(
        name,
        functions,
        arguments,
        &NominalDefinitions::new(),
        limits,
    )
}

/// Invoke an admitted named function with declaration-owned nominal
/// construction plans. The plans are retained by the initial lexical scope
/// and propagated through nested named calls and closures.
pub fn invoke_named_with_nominals(
    name: &str,
    functions: &Functions,
    arguments: &Environment,
    nominal_definitions: &NominalDefinitions,
    limits: Limits,
) -> Result<CanonicalValue, EvaluationError> {
    validate_limits(limits)?;
    let mut context = Context {
        limits,
        steps: 0,
        functions,
        aliases: None,
        session_functions: None,
        repl_bindings: false,
        restrict_function_names: false,
        reject_unhandled_field_calls: false,
        effects: None,
        namespace: function_namespace(name),
        transfer: None,
        cancellation: None,
    };
    context.items(functions.len())?;
    let function = functions.get(name).ok_or_else(|| error("ORNA-EVAL-NAME"))?;
    let supplied = supplied_arguments(arguments, nominal_definitions, &mut context)?;
    let captured = Scope::from_environment_with_nominals(
        &function.environment,
        nominal_definitions,
        &mut context,
    )?;
    invoke_pure(
        &mut context,
        &function.parameters,
        &function.body,
        captured,
        supplied,
        0,
    )
    .and_then(Value::canonical)
}

/// Invoke an admitted function while consuming a caller-owned activation
/// budget. The budget covers argument/default evaluation, the function body,
/// handled effects, and nested calls in one shared accounting context.
pub fn invoke_named_with_effects_and_budget(
    name: &str,
    functions: &Functions,
    arguments: &Environment,
    limits: Limits,
    effects: &mut dyn EffectHandler,
    budget: &mut StepBudget,
) -> Result<CanonicalValue, EvaluationError> {
    validate_limits(limits)?;
    let mut context = Context {
        limits,
        steps: limits.max_steps.saturating_sub(budget.remaining()),
        functions,
        aliases: None,
        session_functions: None,
        repl_bindings: false,
        restrict_function_names: false,
        reject_unhandled_field_calls: false,
        effects: Some(effects),
        namespace: function_namespace(name),
        transfer: None,
        cancellation: None,
    };
    let result = (|| {
        context.items(functions.len())?;
        let function = functions.get(name).ok_or_else(|| error("ORNA-EVAL-NAME"))?;
        let supplied = supplied_arguments(arguments, &NominalDefinitions::new(), &mut context)?;
        let captured = Scope::from_environment(&function.environment, &mut context)?;
        invoke_pure(
            &mut context,
            &function.parameters,
            &function.body,
            captured,
            supplied,
            0,
        )
        .and_then(Value::canonical)
    })();
    *budget = StepBudget::new(limits.max_steps.saturating_sub(context.steps));
    result
}

/// Invokes an admitted function with one effect handler. Nested pure calls and
/// handled effects share the same evaluator resource context.
pub fn invoke_named_with_effects(
    name: &str,
    functions: &Functions,
    arguments: &Environment,
    limits: Limits,
    effects: &mut dyn EffectHandler,
) -> Result<CanonicalValue, EvaluationError> {
    let mut budget = StepBudget::new(limits.max_steps);
    invoke_named_with_effects_and_budget(name, functions, arguments, limits, effects, &mut budget)
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum ArgumentKey {
    Name(String),
    Position(usize),
}

fn parameter_key(parameter: &Parameter, index: usize) -> ArgumentKey {
    match &parameter.pattern {
        Pattern::Name(name, _) if name != "_" => ArgumentKey::Name(name.clone()),
        _ => ArgumentKey::Position(index),
    }
}

fn supplied_arguments(
    arguments: &Environment,
    nominal_definitions: &NominalDefinitions,
    context: &mut Context,
) -> Result<BTreeMap<ArgumentKey, Value>, EvaluationError> {
    Ok(
        Scope::from_environment_with_nominals(arguments, nominal_definitions, context)?
            .0
            .into_iter()
            .map(|(name, value)| (ArgumentKey::Name(name), value))
            .collect(),
    )
}

fn invoke_pure(
    context: &mut Context,
    parameters: &[Parameter],
    body: &Expr,
    mut scope: Scope,
    supplied: BTreeMap<ArgumentKey, Value>,
    depth: usize,
) -> Result<Value, EvaluationError> {
    context.depth(depth)?;
    context.items(parameters.len())?;
    context.items(supplied.len())?;
    let mut keys = BTreeSet::new();
    let mut bound_names = BTreeSet::new();
    for (index, parameter) in parameters.iter().enumerate() {
        let key = parameter_key(parameter, index);
        for name in pattern_names(&parameter.pattern) {
            if name == "_" {
                continue;
            }
            if name.len() > context.limits.max_string_bytes {
                return Err(error("ORNA-EVAL-LIMIT"));
            }
            if !bound_names.insert(name) {
                return Err(error("ORNA-EVAL-ARGUMENT"));
            }
        }
        if !keys.insert(key.clone())
            || (!supplied.contains_key(&key) && parameter.default.is_none())
        {
            return Err(error("ORNA-EVAL-ARGUMENT"));
        }
    }
    if supplied.keys().any(|key| !keys.contains(key)) {
        return Err(error("ORNA-EVAL-ARGUMENT"));
    }
    for (index, parameter) in parameters.iter().enumerate() {
        let value = if let Some(value) = supplied.get(&parameter_key(parameter, index)) {
            value.clone()
        } else {
            context.evaluate(
                parameter
                    .default
                    .as_ref()
                    .expect("omitted defaults were admitted above"),
                &mut scope,
                depth,
            )?
        };
        if !bind(&parameter.pattern, value, &mut scope, context, depth + 1)? {
            return Err(error("ORNA-EVAL-TYPE"));
        }
        context.items(scope.0.len())?;
    }
    let value = context.evaluate(body, &mut scope, depth)?;
    match context.transfer.take() {
        Some(Transfer::Return(value)) => Ok(value),
        // A called function/lambda is a transfer boundary. A loop transfer
        // cannot target a loop outside that boundary.
        Some(Transfer::Break(_) | Transfer::Continue) => Err(error("ORNA-EVAL-UNSUPPORTED")),
        None => Ok(value),
    }
}

fn check_limits(source: &str, limits: Limits) -> Result<(), EvaluationError> {
    validate_limits(limits)?;
    if source.len() > limits.max_source_bytes {
        Err(error("ORNA-EVAL-LIMIT"))
    } else {
        Ok(())
    }
}
fn validate_limits(limits: Limits) -> Result<(), EvaluationError> {
    if limits.max_source_bytes == 0
        || limits.max_steps == 0
        || limits.max_depth == 0
        || limits.max_collection_items == 0
        || limits.max_string_bytes == 0
        || limits.max_integer_digits == 0
    {
        Err(error("ORNA-EVAL-LIMIT"))
    } else {
        Ok(())
    }
}
fn error(code: &'static str) -> EvaluationError {
    // No parser diagnostic, source text, span, input value, or filesystem data
    // crosses this boundary.
    EvaluationError::redacted(SafeText::new(code).expect("static safe code"))
}

/// Encodes the lawful equality identity used by collection and relation
/// distinct folds. The set stores this identity while the stream retains its
/// first original value and therefore its stable order.
fn distinct_identity(value: &Value) -> Result<Vec<u8>, EvaluationError> {
    if value.contains_float() {
        return Err(error("ORNA-EVAL-UNSUPPORTED"));
    }
    value
        .clone()
        .canonical()?
        .encode()
        .map_err(|_| error("ORNA-EVAL-VALUE"))
}

/// Keeps the first original value for each canonical collection identity.
/// Each query invocation constructs its own fold so paired recursive queries
/// cannot suppress one another's anchor or recursive rows.
#[derive(Default)]
struct DistinctValueFold {
    identities: HashSet<Vec<u8>>,
    values: Vec<Value>,
}

impl DistinctValueFold {
    fn insert(&mut self, value: Value) -> Result<bool, EvaluationError> {
        if !self.identities.insert(distinct_identity(&value)?) {
            return Ok(false);
        }
        self.values.push(value);
        Ok(true)
    }

    fn len(&self) -> usize {
        self.values.len()
    }

    fn into_values(self) -> Vec<Value> {
        self.values
    }
}

fn aggregate_task_failures(failures: &[EvaluationError]) -> EvaluationError {
    let Some(primary) = failures.first() else {
        return error("ORNA-EVAL-ERROR");
    };
    if failures.len() == 1 {
        return primary.clone();
    }
    let primary_value = primary.canonical.clone().unwrap_or_else(|| {
        CanonicalErrorValue::new(primary.code(), "<redacted>", [], BTreeMap::new())
            .expect("redacted task error is canonical")
    });
    let mut causes = primary_value.causes();
    for failure in failures.iter().skip(1) {
        causes.push(failure.canonical.clone().unwrap_or_else(|| {
            CanonicalErrorValue::new(failure.code(), "<redacted>", [], BTreeMap::new())
                .expect("redacted task cause is canonical")
        }));
    }
    let aggregate = CanonicalErrorValue::new(
        primary_value.code(),
        primary_value.message(),
        causes,
        primary_value.safe_details(),
    )
    .expect("ordered task error causes remain canonical");
    EvaluationError::from_canonical(aggregate)
}

fn valid_date_literal(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return false;
    }
    let year = value[0..4].parse::<u32>().ok();
    let month = value[5..7].parse::<u32>().ok();
    let day = value[8..10].parse::<u32>().ok();
    let Some((year, month, day)) = year
        .zip(month)
        .zip(day)
        .map(|((year, month), day)| (year, month, day))
    else {
        return false;
    };
    if !(1..=9999).contains(&year) || !(1..=12).contains(&month) {
        return false;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let maximum = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=maximum).contains(&day)
}

/// Parses an Instant literal into the OVB-1 representation defined by the
/// immutable format profile.  The stored seconds are floor-normalised UTC
/// seconds, so fractions before the Unix epoch retain a nonnegative remainder.
fn parse_instant_literal(value: &str) -> Option<(i64, u32)> {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || !bytes.is_ascii()
        || !valid_date_literal(value.get(..10)?)
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return None;
    }
    let component = |start, end| {
        bytes
            .get(start..end)
            .filter(|part| part.iter().all(u8::is_ascii_digit))
            .and_then(|part| std::str::from_utf8(part).ok())
            .and_then(|part| part.parse::<u32>().ok())
    };
    let (hour, minute, second) = (component(11, 13)?, component(14, 16)?, component(17, 19)?);
    if hour >= 24 || minute >= 60 || second >= 60 {
        return None;
    }

    let mut at = 19;
    let nanosecond = if bytes.get(at) == Some(&b'.') {
        at += 1;
        let start = at;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        let digits = at.checked_sub(start)?;
        if !(1..=9).contains(&digits) {
            return None;
        }
        let fraction = component(start, at)?;
        fraction.checked_mul(10u32.pow((9 - digits) as u32))?
    } else {
        0
    };

    let offset_seconds = match bytes.get(at) {
        Some(b'Z') if at + 1 == bytes.len() => 0i64,
        Some(b'+' | b'-') if at + 6 == bytes.len() && bytes.get(at + 3) == Some(&b':') => {
            let hours = component(at + 1, at + 3)?;
            let minutes = component(at + 4, at + 6)?;
            if hours > 23 || minutes >= 60 {
                return None;
            }
            let seconds = i64::from(hours) * 3_600 + i64::from(minutes) * 60;
            if bytes[at] == b'+' { seconds } else { -seconds }
        }
        _ => return None,
    };

    let year = component(0, 4)?;
    let month = component(5, 7)?;
    let day = component(8, 10)?;
    let days = days_since_unix_epoch(year, month, day)?;
    let local_seconds = days
        .checked_mul(86_400)?
        .checked_add(i64::from(hour) * 3_600 + i64::from(minute) * 60 + i64::from(second))?;
    local_seconds
        .checked_sub(offset_seconds)
        .map(|seconds| (seconds, nanosecond))
}

fn days_since_unix_epoch(year: u32, month: u32, day: u32) -> Option<i64> {
    if !valid_date_literal(&format!("{year:04}-{month:02}-{day:02}")) {
        return None;
    }
    let year = i64::from(year);
    let month = i64::from(month);
    let day = i64::from(day);
    let leap_days = |year: i64| year / 4 - year / 100 + year / 400;
    let years_before = year - 1;
    let days_before_year = years_before * 365 + leap_days(years_before);
    let days_before_month =
        match month {
            1 => 0,
            2 => 31,
            3 => 59,
            4 => 90,
            5 => 120,
            6 => 151,
            7 => 181,
            8 => 212,
            9 => 243,
            10 => 273,
            11 => 304,
            12 => 334,
            _ => return None,
        } + if month > 2 && year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
            1
        } else {
            0
        };
    let unix_epoch_days = 719_162;
    Some(days_before_year + days_before_month + day - 1 - unix_epoch_days)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProviderRetryPolicy {
    max_attempts: usize,
    initial_delay_nanoseconds: BigInt,
    max_delay_nanoseconds: BigInt,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProviderStream {
    sources: Vec<StreamSourceCursor>,
    buffer_capacity: usize,
    batch_size: Option<usize>,
    throttle_nanoseconds: Option<BigInt>,
    debounce_nanoseconds: Option<BigInt>,
    retry: Option<ProviderRetryPolicy>,
    recover: Option<Box<Value>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Value {
    Null,
    Unit,
    Bool(bool),
    Int(BigInt),
    Decimal(DecimalValue),
    Money {
        amount: DecimalValue,
        currency: [u8; 16],
    },
    Float(u64),
    String(String),
    Blob(Vec<u8>),
    Date(String),
    Uuid([u8; 16]),
    /// OVB's complete stored identity is retained, including its snapshot pin.
    Reference(CanonicalValue),
    Instant {
        unix_seconds: i64,
        nanosecond: u32,
    },
    Duration {
        seconds: BigInt,
        nanosecond: u32,
    },
    /// Calendar periods remain distinct from elapsed durations.
    Period {
        days: BigInt,
    },
    Error(EvaluationError),
    Range {
        lower: Option<Box<Value>>,
        upper: Option<Box<Value>>,
        upper_inclusive: bool,
    },
    List(Vec<Value>),
    /// Immutable replayable source over a finite list. `position` is the
    /// `orna.list.v1` index of the next item; the digest binds the source
    /// label to the canonical typed encoding of the complete list.
    Stream {
        values: Vec<Value>,
        source_label: String,
        source_digest: [u8; 32],
        position: usize,
        provider: Option<ProviderStream>,
    },
    Relation(RelationPlan),
    Tuple(Vec<Value>),
    Record(BTreeMap<String, Value>),
    NominalRecord {
        type_id: Raw,
        fields: Vec<(Raw, Value)>,
    },
    Enum {
        type_id: Raw,
        variant_id: Raw,
        payload: Option<Box<Value>>,
    },
    Option(Option<Box<Value>>),
    Function {
        name: String,
        nominal_definitions: NominalDefinitions,
    },
    Closure(Arc<Closure>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Closure {
    parameters: Vec<Parameter>,
    body: Expr,
    captured: Scope,
    namespace: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DecimalValue {
    coefficient: BigInt,
    exponent10: BigInt,
}
impl DecimalValue {
    fn new(mut coefficient: BigInt, mut exponent10: BigInt) -> Result<Self, EvaluationError> {
        if coefficient.is_zero() {
            return Ok(Self {
                coefficient,
                exponent10: BigInt::zero(),
            });
        }
        while (&coefficient % 10u8).is_zero() {
            coefficient /= 10u8;
            exponent10 += 1;
        }
        if exponent10
            .abs()
            .to_usize()
            .is_none_or(|value| value > DEFAULT_INTEGER_DIGITS)
        {
            return Err(error("ORNA-EVAL-LIMIT"));
        }
        Ok(Self {
            coefficient,
            exponent10,
        })
    }
    fn add(&self, other: &Self) -> Result<Self, EvaluationError> {
        let exponent = self.exponent10.clone().min(other.exponent10.clone());
        let left = (&self.exponent10 - &exponent)
            .to_usize()
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        let right = (&other.exponent10 - &exponent)
            .to_usize()
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        if left > DEFAULT_INTEGER_DIGITS || right > DEFAULT_INTEGER_DIGITS {
            return Err(error("ORNA-EVAL-LIMIT"));
        }
        Self::new(
            &self.coefficient * BigInt::from(10u8).pow(left as u32)
                + &other.coefficient * BigInt::from(10u8).pow(right as u32),
            exponent,
        )
    }
    fn multiply(&self, other: &Self) -> Result<Self, EvaluationError> {
        Self::new(
            &self.coefficient * &other.coefficient,
            &self.exponent10 + &other.exponent10,
        )
    }
    fn divide(&self, other: &Self) -> Result<Self, EvaluationError> {
        if other.coefficient.is_zero() {
            return Err(error("ORNA-EVAL-DIVIDE-BY-ZERO"));
        }
        let gcd = self.coefficient.gcd(&other.coefficient);
        let mut numerator = &self.coefficient / &gcd;
        let mut denominator = (&other.coefficient / gcd).abs();
        if other.coefficient.sign() == Sign::Minus {
            numerator = -numerator;
        }
        let mut twos = 0usize;
        let mut fives = 0usize;
        while (&denominator % 2u8).is_zero() {
            denominator /= 2u8;
            twos += 1;
        }
        while (&denominator % 5u8).is_zero() {
            denominator /= 5u8;
            fives += 1;
        }
        if denominator != BigInt::from(1) {
            return Err(error("InexactDivision"));
        }
        let scale = twos.max(fives);
        if scale > DEFAULT_INTEGER_DIGITS {
            return Err(error("ORNA-EVAL-LIMIT"));
        }
        numerator *= BigInt::from(2u8).pow((scale - twos) as u32);
        numerator *= BigInt::from(5u8).pow((scale - fives) as u32);
        Self::new(
            numerator,
            &self.exponent10 - &other.exponent10 - BigInt::from(scale),
        )
    }
    fn divide_rounded(&self, other: &Self, scale: usize) -> Result<Self, EvaluationError> {
        if other.coefficient.is_zero() {
            return Err(error("ORNA-EVAL-DIVIDE-BY-ZERO"));
        }
        if scale > DEFAULT_INTEGER_DIGITS {
            return Err(error("ORNA-EVAL-LIMIT"));
        }
        let gcd = self.coefficient.gcd(&other.coefficient);
        let mut numerator = &self.coefficient / &gcd;
        let mut denominator = (&other.coefficient / gcd).abs();
        if other.coefficient.sign() == Sign::Minus {
            numerator = -numerator;
        }
        let power = &self.exponent10 - &other.exponent10 + BigInt::from(scale);
        if power.sign() == Sign::Plus || power.is_zero() {
            let power = power.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
            if power > DEFAULT_INTEGER_DIGITS {
                return Err(error("ORNA-EVAL-LIMIT"));
            }
            numerator *= BigInt::from(10u8).pow(power as u32);
        } else {
            let power = (-power)
                .to_usize()
                .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
            if power > DEFAULT_INTEGER_DIGITS {
                return Err(error("ORNA-EVAL-LIMIT"));
            }
            denominator *= BigInt::from(10u8).pow(power as u32);
        }
        let (mut quotient, remainder) = numerator.div_rem(&denominator);
        if !remainder.is_zero() {
            let twice = remainder.abs() * 2u8;
            if twice > denominator
                || (twice == denominator && (&quotient % 2u8).abs() == BigInt::from(1u8))
            {
                if numerator.sign() == Sign::Minus {
                    quotient -= 1;
                } else {
                    quotient += 1;
                }
            }
        }
        Self::new(quotient, -BigInt::from(scale))
    }

    fn round_to_scale(&self, scale: usize) -> Result<Self, EvaluationError> {
        if scale > DEFAULT_INTEGER_DIGITS {
            return Err(error("ORNA-EVAL-LIMIT"));
        }
        let shift = &self.exponent10 + BigInt::from(scale);
        let mut coefficient = self.coefficient.clone();
        if shift.sign() == Sign::Plus || shift.is_zero() {
            let shift = shift.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
            if shift > DEFAULT_INTEGER_DIGITS {
                return Err(error("ORNA-EVAL-LIMIT"));
            }
            coefficient *= BigInt::from(10u8).pow(shift as u32);
        } else {
            let shift = (-shift)
                .to_usize()
                .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
            if shift > DEFAULT_INTEGER_DIGITS {
                return Err(error("ORNA-EVAL-LIMIT"));
            }
            let divisor = BigInt::from(10u8).pow(shift as u32);
            let (mut quotient, remainder) = coefficient.div_rem(&divisor);
            if !remainder.is_zero() {
                let twice = remainder.abs() * 2u8;
                if twice > divisor
                    || (twice == divisor && (&quotient % 2u8).abs() == BigInt::from(1u8))
                {
                    if coefficient.sign() == Sign::Minus {
                        quotient -= 1;
                    } else {
                        quotient += 1;
                    }
                }
            }
            coefficient = quotient;
        }
        Self::new(coefficient, -BigInt::from(scale))
    }
}

impl Value {
    fn contains_callable(&self) -> bool {
        match self {
            Self::Function { .. } | Self::Closure(_) => true,
            Self::List(values) | Self::Tuple(values) => values.iter().any(Self::contains_callable),
            Self::Stream {
                values, provider, ..
            } => {
                values.iter().any(Self::contains_callable)
                    || provider
                        .as_ref()
                        .and_then(|provider| provider.recover.as_deref())
                        .is_some_and(Self::contains_callable)
            }
            Self::Record(values) => values.values().any(Self::contains_callable),
            Self::NominalRecord { fields, .. } => {
                fields.iter().any(|(_, value)| value.contains_callable())
            }
            Self::Enum { payload, .. } | Self::Option(payload) => payload
                .as_ref()
                .is_some_and(|value| value.contains_callable()),
            Self::Relation(_) => true,
            _ => false,
        }
    }
    fn contains_float(&self) -> bool {
        match self {
            Self::Float(_) => true,
            Self::List(values) | Self::Tuple(values) => values.iter().any(Self::contains_float),
            Self::Stream {
                values, provider, ..
            } => {
                values.iter().any(Self::contains_float)
                    || provider
                        .as_ref()
                        .and_then(|provider| provider.recover.as_deref())
                        .is_some_and(Self::contains_float)
            }
            Self::Record(values) => values.values().any(Self::contains_float),
            Self::NominalRecord { fields, .. } => {
                fields.iter().any(|(_, value)| value.contains_float())
            }
            Self::Enum { payload, .. } | Self::Option(payload) => {
                payload.as_ref().is_some_and(|value| value.contains_float())
            }
            _ => false,
        }
    }
    fn canonical(self) -> Result<CanonicalValue, EvaluationError> {
        if let Self::Error(failure) = self {
            return Err(failure);
        }
        CanonicalValue::new(self.raw()?).map_err(|_| error("ORNA-EVAL-VALUE"))
    }
    fn raw(self) -> Result<Raw, EvaluationError> {
        Ok(match self {
            Self::Function { .. } | Self::Closure(_) => {
                return Err(error("ORNA-EVAL-UNSUPPORTED"));
            }
            Self::Relation(_) | Self::Period { .. } => {
                return Err(error("ORNA-EVAL-UNSUPPORTED"));
            }
            // Error values are only available to the handling side of `|?`.
            // They must not cross the successful canonical-value boundary,
            // but a containing value must preserve the original failure while
            // propagating to that boundary.
            Self::Error(failure) => return Err(failure),
            Self::Null => Raw::Null,
            Self::Unit => Raw::Tag(60014, Box::new(Raw::Array(vec![]))),
            Self::Bool(value) => Raw::Bool(value),
            Self::Int(value) => Raw::Int(value),
            Self::Decimal(value) => Raw::Tag(
                60000,
                Box::new(Raw::Array(vec![
                    Raw::Int(value.coefficient),
                    Raw::Int(value.exponent10),
                ])),
            ),
            Self::Money { amount, currency } => Raw::Tag(
                60007,
                Box::new(Raw::Array(vec![
                    Raw::Tag(
                        60000,
                        Box::new(Raw::Array(vec![
                            Raw::Int(amount.coefficient),
                            Raw::Int(amount.exponent10),
                        ])),
                    ),
                    object_id_raw(currency),
                ])),
            ),
            Self::Float(bits) => Raw::Float(bits),
            Self::String(value) => Raw::Text(value),
            Self::Blob(value) => Raw::Bytes(value),
            Self::Date(value) => Raw::Tag(60001, Box::new(Raw::Text(value))),
            Self::Uuid(value) => object_id_raw(value),
            Self::Reference(value) => value.raw().clone(),
            Self::Instant {
                unix_seconds,
                nanosecond,
            } => Raw::Tag(
                60002,
                Box::new(Raw::Array(vec![
                    Raw::Int(unix_seconds.into()),
                    Raw::Int(nanosecond.into()),
                ])),
            ),
            Self::Duration {
                seconds,
                nanosecond,
            } => Raw::Tag(
                60005,
                Box::new(Raw::Array(vec![
                    Raw::Int(seconds),
                    Raw::Int(nanosecond.into()),
                ])),
            ),
            Self::Range {
                lower,
                upper,
                upper_inclusive,
            } => Raw::Tag(
                60019,
                Box::new(Raw::Array(vec![
                    raw_option_value(lower)?,
                    raw_option_value(upper)?,
                    Raw::Bool(upper_inclusive),
                ])),
            ),
            Self::List(values) => Raw::Array(
                values
                    .into_iter()
                    .map(Value::raw)
                    .collect::<Result<_, _>>()?,
            ),
            Self::Tuple(values) if values.is_empty() => {
                Raw::Tag(60014, Box::new(Raw::Array(Vec::new())))
            }
            Self::Tuple(values) => Raw::Tag(
                60015,
                Box::new(Raw::Array(
                    values
                        .into_iter()
                        .map(Value::raw)
                        .collect::<Result<_, _>>()?,
                )),
            ),
            // The finite stream's source identity and digest remain in the
            // live evaluator value; the canonical boundary exposes its real
            // item sequence rather than substituting a placeholder result.
            Self::Stream {
                values,
                position,
                provider,
                ..
            } => {
                if provider.is_some() {
                    return Err(error("ORNA-EVAL-UNSUPPORTED"));
                }
                Raw::Array(
                    values
                        .into_iter()
                        .skip(position)
                        .map(Value::raw)
                        .collect::<Result<_, _>>()?,
                )
            }
            Self::Record(values) => {
                // OVB orders map keys by complete deterministic encodings.
                // Keep the comparator tied to that rule, including CBOR head
                // length transitions and multi-byte Unicode field names.
                let mut fields = values
                    .into_iter()
                    .map(|(key, value)| {
                        if !key.nfc().eq(key.chars()) {
                            return Err(error("ORNA-EVAL-VALUE"));
                        }
                        let encoded_key = CanonicalValue::new(Raw::Text(key.clone()))
                            .map_err(|_| error("ORNA-EVAL-VALUE"))?
                            .encode()
                            .map_err(|_| error("ORNA-EVAL-VALUE"))?;
                        Ok((encoded_key, key, value))
                    })
                    .collect::<Result<Vec<_>, EvaluationError>>()?;
                fields.sort_by(|(left, _, _), (right, _, _)| left.cmp(right));
                Raw::Map(
                    fields
                        .into_iter()
                        .map(|(_, key, value)| value.raw().map(|value| (Raw::Text(key), value)))
                        .collect::<Result<_, _>>()?,
                )
            }
            Self::NominalRecord { type_id, fields } => {
                // The runtime value preserves declaration order. OVB requires
                // canonical field-key order only at the serialization edge.
                let mut encoded = fields
                    .into_iter()
                    .map(|(key, value)| {
                        let encoded_key = CanonicalValue::new(key.clone())
                            .map_err(|_| error("ORNA-EVAL-VALUE"))?
                            .encode()
                            .map_err(|_| error("ORNA-EVAL-VALUE"))?;
                        Ok((encoded_key, key, value))
                    })
                    .collect::<Result<Vec<_>, EvaluationError>>()?;
                encoded.sort_by(|(left, _, _), (right, _, _)| left.cmp(right));
                Raw::Tag(
                    60009,
                    Box::new(Raw::Array(vec![
                        type_id,
                        Raw::Array(
                            encoded
                                .into_iter()
                                .map(|(_, key, value)| {
                                    value.raw().map(|value| Raw::Array(vec![key, value]))
                                })
                                .collect::<Result<_, _>>()?,
                        ),
                    ])),
                )
            }
            Self::Enum {
                type_id,
                variant_id,
                payload,
            } => Raw::Tag(
                60008,
                Box::new(Raw::Array(vec![
                    type_id,
                    variant_id,
                    payload
                        .map(|value| value.raw())
                        .transpose()?
                        .unwrap_or(Raw::Null),
                ])),
            ),
            Self::Option(value) => Raw::Tag(
                60013,
                Box::new(match value {
                    Some(value) => Raw::Array(vec![Raw::Int(1.into()), value.raw()?]),
                    None => Raw::Array(vec![Raw::Int(0.into())]),
                }),
            ),
        })
    }
    fn from_canonical(
        value: &CanonicalValue,
        context: &mut Context,
        depth: usize,
    ) -> Result<Self, EvaluationError> {
        context.depth(depth)?;
        match value.raw() {
            Raw::Null => Ok(Self::Null),
            Raw::Tag(60014, boxed) if matches!(boxed.as_ref(), Raw::Array(values) if values.is_empty()) => {
                Ok(Self::Unit)
            }
            Raw::Bool(value) => Ok(Self::Bool(*value)),
            Raw::Int(value) => context.integer(value.clone()).map(Self::Int),
            Raw::Float(bits) => Ok(Self::Float(*bits)),
            Raw::Text(value) => context.string(value.clone()).map(Self::String),
            Raw::Bytes(value) => Ok(Self::Blob(value.clone())),
            Raw::Tag(60001, boxed) => {
                let Raw::Text(value) = boxed.as_ref() else {
                    return Err(error("ORNA-EVAL-VALUE"));
                };
                if !valid_date_literal(value) {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                context.string(value.clone()).map(Self::Date)
            }
            Raw::Tag(37, boxed) => {
                let Raw::Bytes(bytes) = boxed.as_ref() else {
                    return Err(error("ORNA-EVAL-VALUE"));
                };
                let bytes = bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| error("ORNA-EVAL-VALUE"))?;
                Ok(Self::Uuid(bytes))
            }
            Raw::Tag(60010, _) => Ok(Self::Reference(value.clone())),
            Raw::Tag(60002, boxed) => Self::instant_from_raw(boxed, context),
            Raw::Tag(60005, boxed) => Self::duration_from_raw(boxed, context),
            Raw::Array(values) => {
                context.items(values.len())?;
                values
                    .iter()
                    .map(|value| Self::from_raw(value, context, depth + 1))
                    .collect::<Result<Vec<_>, _>>()
                    .map(Self::List)
            }
            Raw::Tag(60015, boxed) => {
                let Raw::Array(values) = boxed.as_ref() else {
                    return Err(error("ORNA-EVAL-VALUE"));
                };
                if values.is_empty() {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                context.items(values.len())?;
                values
                    .iter()
                    .map(|value| Self::from_raw(value, context, depth + 1))
                    .collect::<Result<Vec<_>, _>>()
                    .map(Self::Tuple)
            }
            Raw::Map(values) => {
                context.items(values.len())?;
                let mut record = BTreeMap::new();
                for (key, value) in values {
                    let Raw::Text(key) = key else {
                        return Err(error("ORNA-EVAL-UNSUPPORTED"));
                    };
                    if !key.nfc().eq(key.chars()) {
                        return Err(error("ORNA-EVAL-VALUE"));
                    }
                    context.string(key.clone())?;
                    if record
                        .insert(key.clone(), Self::from_raw(value, context, depth + 1)?)
                        .is_some()
                    {
                        return Err(error("ORNA-EVAL-VALUE"));
                    }
                }
                Ok(Self::Record(record))
            }
            Raw::Tag(60000, boxed) => Self::decimal_from_raw(boxed, context),
            Raw::Tag(60007, boxed) => Self::money_from_raw(boxed, context),
            Raw::Tag(60008, boxed) => Self::enum_from_raw(boxed, context, depth),
            Raw::Tag(60009, boxed) => Self::nominal_record_from_raw(boxed, context, depth),
            Raw::Tag(60013, boxed) => Self::option_from_raw(boxed, context, depth),
            Raw::Tag(60019, boxed) => Self::range_from_raw(boxed, context, depth),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn from_raw(raw: &Raw, context: &mut Context, depth: usize) -> Result<Self, EvaluationError> {
        let value = CanonicalValue::new(raw.clone()).map_err(|_| error("ORNA-EVAL-VALUE"))?;
        Self::from_canonical(&value, context, depth)
    }
    fn decimal_from_raw(raw: &Raw, context: &mut Context) -> Result<Self, EvaluationError> {
        let Raw::Array(parts) = raw else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        let [Raw::Int(coefficient), Raw::Int(exponent)] = parts.as_slice() else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        context.integer(coefficient.clone())?;
        context.integer(exponent.clone())?;
        DecimalValue::new(coefficient.clone(), exponent.clone()).map(Self::Decimal)
    }
    fn money_from_raw(raw: &Raw, context: &mut Context) -> Result<Self, EvaluationError> {
        let Raw::Array(parts) = raw else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        let [Raw::Tag(60000, decimal_raw), currency_raw] = parts.as_slice() else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        let Self::Decimal(amount) = Self::decimal_from_raw(decimal_raw, context)? else {
            unreachable!("decimal decoder returns Decimal");
        };
        let canonical_decimal = Raw::Tag(
            60000,
            Box::new(Raw::Array(vec![
                Raw::Int(amount.coefficient.clone()),
                Raw::Int(amount.exponent10.clone()),
            ])),
        );
        if canonical_decimal != Raw::Tag(60000, decimal_raw.clone()) {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        let currency = object_id_key(currency_raw).ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
        Ok(Self::Money { amount, currency })
    }
    fn instant_from_raw(raw: &Raw, context: &mut Context) -> Result<Self, EvaluationError> {
        let Raw::Array(parts) = raw else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        let [Raw::Int(seconds), Raw::Int(nanosecond)] = parts.as_slice() else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        context.integer(seconds.clone())?;
        context.integer(nanosecond.clone())?;
        let unix_seconds = seconds.to_i64().ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
        let nanosecond = nanosecond
            .to_u32()
            .filter(|nanosecond| *nanosecond < 1_000_000_000)
            .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
        Ok(Self::Instant {
            unix_seconds,
            nanosecond,
        })
    }
    fn duration_from_raw(raw: &Raw, context: &mut Context) -> Result<Self, EvaluationError> {
        let Raw::Array(parts) = raw else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        let [Raw::Int(seconds), Raw::Int(nanosecond)] = parts.as_slice() else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        context.integer(seconds.clone())?;
        context.integer(nanosecond.clone())?;
        let nanosecond = nanosecond
            .to_u32()
            .filter(|nanosecond| *nanosecond < 1_000_000_000)
            .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
        Ok(Self::Duration {
            seconds: seconds.clone(),
            nanosecond,
        })
    }
    fn enum_from_raw(
        raw: &Raw,
        context: &mut Context,
        depth: usize,
    ) -> Result<Self, EvaluationError> {
        let Raw::Array(parts) = raw else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        let [type_id, variant_id, payload] = parts.as_slice() else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        let payload = match payload {
            Raw::Null => None,
            value => Some(Box::new(Self::from_raw(value, context, depth + 1)?)),
        };
        Ok(Self::Enum {
            type_id: type_id.clone(),
            variant_id: variant_id.clone(),
            payload,
        })
    }
    fn nominal_record_from_raw(
        raw: &Raw,
        context: &mut Context,
        depth: usize,
    ) -> Result<Self, EvaluationError> {
        let Raw::Array(parts) = raw else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        let [type_id, Raw::Array(raw_fields)] = parts.as_slice() else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        let nominal = !matches!(type_id, Raw::Null);
        if nominal && !is_object_id_raw(type_id) {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        context.items(raw_fields.len())?;
        let mut field_ids = BTreeSet::new();
        let mut fields = Vec::with_capacity(raw_fields.len());
        for field in raw_fields {
            let Raw::Array(parts) = field else {
                return Err(error("ORNA-EVAL-VALUE"));
            };
            let [key, value] = parts.as_slice() else {
                return Err(error("ORNA-EVAL-VALUE"));
            };
            if nominal {
                let Some(field_id) = object_id_key(key) else {
                    return Err(error("ORNA-EVAL-VALUE"));
                };
                if !field_ids.insert(field_id) {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
            } else if let Raw::Text(name) = key {
                context.string(name.clone())?;
            } else {
                return Err(error("ORNA-EVAL-VALUE"));
            }
            fields.push((key.clone(), Self::from_raw(value, context, depth + 1)?));
        }
        Ok(Self::NominalRecord {
            type_id: type_id.clone(),
            fields,
        })
    }
    fn option_from_raw(
        raw: &Raw,
        context: &mut Context,
        depth: usize,
    ) -> Result<Self, EvaluationError> {
        let Raw::Array(parts) = raw else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        match parts.as_slice() {
            [Raw::Int(tag)] if tag.is_zero() => Ok(Self::Option(None)),
            [Raw::Int(tag), value] if *tag == BigInt::from(1) => Ok(Self::Option(Some(Box::new(
                Self::from_raw(value, context, depth + 1)?,
            )))),
            _ => Err(error("ORNA-EVAL-VALUE")),
        }
    }
    fn range_from_raw(
        raw: &Raw,
        context: &mut Context,
        depth: usize,
    ) -> Result<Self, EvaluationError> {
        let Raw::Array(parts) = raw else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        let [lower, upper, Raw::Bool(upper_inclusive)] = parts.as_slice() else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        let lower = option_value(lower, context, depth + 1)?;
        let upper = option_value(upper, context, depth + 1)?;
        validate_range_endpoints(lower.as_deref(), upper.as_deref())?;
        Ok(Self::Range {
            lower,
            upper,
            upper_inclusive: *upper_inclusive,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Scope(
    BTreeMap<String, Value>,
    BTreeSet<String>,
    BTreeSet<String>,
    NominalDefinitions,
    BTreeSet<String>,
);
impl Scope {
    fn from_environment(
        environment: &Environment,
        context: &mut Context,
    ) -> Result<Self, EvaluationError> {
        Self::from_environment_with_nominals(environment, &NominalDefinitions::new(), context)
    }

    fn from_environment_with_nominals(
        environment: &Environment,
        nominal_definitions: &NominalDefinitions,
        context: &mut Context,
    ) -> Result<Self, EvaluationError> {
        validate_nominal_definitions(nominal_definitions, context.limits)?;
        context.items(environment.len())?;
        context.items(nominal_definitions.len())?;
        let mut values = BTreeMap::new();
        for (name, value) in environment {
            if name.len() > context.limits.max_string_bytes {
                return Err(error("ORNA-EVAL-LIMIT"));
            }
            let value = Value::from_canonical(value, context, 0)?;
            validate_admitted_nominals(&value, nominal_definitions, context.limits)?;
            values.insert(name.clone(), value);
        }
        Ok(Self(
            values,
            BTreeSet::new(),
            BTreeSet::new(),
            nominal_definitions.clone(),
            BTreeSet::new(),
        ))
    }
}

struct Context<'functions, 'effects> {
    limits: Limits,
    steps: u64,
    functions: &'functions Functions,
    aliases: Option<&'functions BTreeMap<String, String>>,
    session_functions: Option<&'functions BTreeSet<String>>,
    repl_bindings: bool,
    restrict_function_names: bool,
    reject_unhandled_field_calls: bool,
    effects: Option<&'effects mut dyn EffectHandler>,
    namespace: Option<String>,
    transfer: Option<Transfer>,
    cancellation: Option<&'functions CancellationToken>,
}

/// An evaluator-only non-local control transfer. It never crosses the public
/// canonical-value boundary: a function consumes `Return`, and a finite `for`
/// consumes `Break` or `Continue`.
enum Transfer {
    Return(Value),
    Break(Value),
    Continue,
}

enum RelationRow {
    Skip,
    Yield(Value),
    End,
}

/// A downstream stage can reject a value after an earlier `take` has already
/// emitted its full result. Preserve the rejection while also closing the
/// source scan so the next page is not read just to rediscover that bound.
fn rejected_relation_rows(
    stages: &[RelationStage],
    counters: &[usize],
    stage_offset: usize,
) -> Vec<RelationRow> {
    let mut rows = vec![RelationRow::Skip];
    if stages.iter().enumerate().any(|(offset, stage)| {
        matches!(stage, RelationStage::Take(count) if counters[stage_offset + offset] >= *count)
    }) {
        rows.push(RelationRow::End);
    }
    rows
}

#[derive(Clone, Debug)]
struct ReadyStreamItem {
    source: String,
    identity: [u8; 32],
    checkpoint: Vec<u8>,
    event_time: Instant,
    value: Value,
    recovery_error: Option<EvaluationError>,
}

fn stream_elapsed_nanoseconds(later: Instant, earlier: Instant) -> Option<BigInt> {
    if later < earlier {
        return None;
    }
    let seconds = BigInt::from(later.unix_seconds) - BigInt::from(earlier.unix_seconds);
    let nanoseconds = BigInt::from(later.nanosecond) - BigInt::from(earlier.nanosecond);
    Some(seconds * BigInt::from(1_000_000_000u64) + nanoseconds)
}

fn invoke_task_callback(
    callback: Value,
    functions: &Functions,
    aliases: Option<&BTreeMap<String, String>>,
    session_functions: Option<&BTreeSet<String>>,
    limits: Limits,
    cancellation: CancellationToken,
    repl_bindings: bool,
    restrict_function_names: bool,
    reject_unhandled_field_calls: bool,
    depth: usize,
    mut effects: Option<Box<dyn EffectHandler + Send>>,
) -> Result<Value, EvaluationError> {
    let mut context = Context {
        limits,
        steps: 0,
        functions,
        aliases,
        session_functions,
        repl_bindings,
        restrict_function_names,
        reject_unhandled_field_calls,
        effects: effects
            .as_deref_mut()
            .map(|handler| handler as &mut dyn EffectHandler),
        namespace: None,
        transfer: None,
        cancellation: Some(&cancellation),
    };
    context.invoke_callable(&callback, Vec::new(), depth)
}

/// Run finite callback lists with a bounded number of owned worker threads.
/// A first ordinary failure stops admission for `parallel`; a first success
/// stops admission for `race`. In-flight children observe the shared token,
/// and `thread::scope` joins every worker before the owner returns.
fn run_task_callbacks(
    callbacks: &[Value],
    mut child_effects: Vec<Option<Box<dyn EffectHandler + Send>>>,
    callback_depth: usize,
    parent: Option<CancellationToken>,
    functions: &Functions,
    aliases: Option<&BTreeMap<String, String>>,
    session_functions: Option<&BTreeSet<String>>,
    limits: Limits,
    repl_bindings: bool,
    restrict_function_names: bool,
    reject_unhandled_field_calls: bool,
    cancel_after_failure: bool,
    cancel_after_success: bool,
) -> (Vec<Option<Result<Value, EvaluationError>>>, Option<Value>) {
    const MAX_TASK_WORKERS: usize = 32;
    let available = std::thread::available_parallelism().map_or(1, usize::from);
    let worker_count = callbacks.len().min(available).min(MAX_TASK_WORKERS).max(1);
    let next = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let shared_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let child_effects = Arc::new(std::sync::Mutex::new(std::mem::take(&mut child_effects)));

    std::thread::scope(|scope| {
        let (sender, receiver) = std::sync::mpsc::channel();
        for _ in 0..worker_count {
            let sender = sender.clone();
            let next = Arc::clone(&next);
            let shared_cancel = Arc::clone(&shared_cancel);
            let child_effects = Arc::clone(&child_effects);
            let parent = parent.clone();
            scope.spawn(move || {
                loop {
                    if shared_cancel.load(std::sync::atomic::Ordering::Acquire)
                        || parent.as_ref().is_some_and(CancellationToken::is_requested)
                    {
                        break;
                    }
                    let index = next.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    let Some(callback) = callbacks.get(index).cloned() else {
                        break;
                    };
                    let effects = child_effects
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .get_mut(index)
                        .and_then(Option::take);
                    let cancellation =
                        CancellationToken::child(Arc::clone(&shared_cancel), parent.clone());
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        invoke_task_callback(
                            callback,
                            functions,
                            aliases,
                            session_functions,
                            limits,
                            cancellation,
                            repl_bindings,
                            restrict_function_names,
                            reject_unhandled_field_calls,
                            callback_depth,
                            effects,
                        )
                    }))
                    .unwrap_or_else(|_| Err(error("ORNA-EVAL-ERROR")));
                    if (cancel_after_failure
                        && result
                            .as_ref()
                            .is_err_and(|failure| failure.code() != "ORNA-EVAL-CANCELLED"))
                        || (cancel_after_success && result.is_ok())
                    {
                        shared_cancel.store(true, std::sync::atomic::Ordering::Release);
                    }
                    let _ = sender.send((index, result));
                }
            });
        }
        drop(sender);
        let mut results = (0..callbacks.len()).map(|_| None).collect::<Vec<_>>();
        let mut winner = None;
        while let Ok((index, result)) = receiver.recv() {
            if cancel_after_success
                && winner.is_none()
                && let Ok(value) = &result
            {
                winner = Some(value.clone());
                shared_cancel.store(true, std::sync::atomic::Ordering::Release);
            }
            results[index] = Some(result);
        }
        (results, winner)
    })
}

impl Context<'_, '_> {
    fn effect_value(&mut self, value: &CanonicalValue) -> Result<Value, EvaluationError> {
        // Effect results cross a canonical boundary. Apply the depth limit to
        // the returned value's own structure from its root, independently of
        // the source call stack. Otherwise a handler can commit an effect and
        // result decoding can then fail only because that caller was deep.
        Value::from_canonical(value, self, 0)
    }

    fn step(&mut self) -> Result<(), EvaluationError> {
        if let Some(cancellation) = self.cancellation {
            cancellation.check()?;
        }
        self.steps = self
            .steps
            .checked_add(1)
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        if self.steps > self.limits.max_steps {
            Err(error("ORNA-EVAL-LIMIT"))
        } else {
            Ok(())
        }
    }
    fn depth(&self, depth: usize) -> Result<(), EvaluationError> {
        if depth > self.limits.max_depth {
            Err(error("ORNA-EVAL-LIMIT"))
        } else {
            Ok(())
        }
    }
    fn items(&self, count: usize) -> Result<(), EvaluationError> {
        if count > self.limits.max_collection_items {
            Err(error("ORNA-EVAL-LIMIT"))
        } else {
            Ok(())
        }
    }
    fn string(&self, value: String) -> Result<String, EvaluationError> {
        if value.len() > self.limits.max_string_bytes {
            Err(error("ORNA-EVAL-LIMIT"))
        } else {
            Ok(value)
        }
    }
    fn error_field(
        &mut self,
        failure: &EvaluationError,
        name: &str,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.depth(depth)?;
        let canonical = failure
            .canonical
            .as_ref()
            .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
        match name {
            "code" => self.string(canonical.code().to_owned()).map(Value::String),
            "message" => self
                .string(canonical.message().to_owned())
                .map(Value::String),
            "causes" => {
                let causes = canonical.causes();
                self.items(causes.len())?;
                Ok(Value::List(
                    causes
                        .into_iter()
                        .map(|cause| Value::Error(EvaluationError::from_canonical(cause)))
                        .collect(),
                ))
            }
            "safe_details" => {
                let details = canonical.safe_details();
                self.items(details.len())?;
                let mut fields = BTreeMap::new();
                for (key, value) in details {
                    self.string(key.clone())?;
                    let canonical = CanonicalValue::new(value.raw().clone())
                        .map_err(|_| error("ORNA-EVAL-VALUE"))?;
                    let value = Value::from_canonical(&canonical, self, depth + 1)?;
                    fields.insert(key, value);
                }
                Ok(Value::Record(fields))
            }
            _ => Err(error("ORNA-EVAL-FIELD")),
        }
    }

    fn reference_field(
        &mut self,
        reference: &CanonicalValue,
        name: &str,
        scope: &Scope,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.depth(depth)?;
        let Raw::Tag(60010, payload) = reference.raw() else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        let Raw::Array(identity) = payload.as_ref() else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        let [_, _, key, _] = identity.as_slice() else {
            return Err(error("ORNA-EVAL-VALUE"));
        };

        // The canonical key is stored in the reference itself, so reading it
        // does not fetch or materialize the referenced row.
        if name == "key" {
            let key = CanonicalValue::new(key.clone()).map_err(|_| error("ORNA-EVAL-VALUE"))?;
            return Value::from_canonical(&key, self, depth + 1);
        }

        let remaining = self.limits.max_steps.saturating_sub(self.steps);
        let mut budget = StepBudget::new(remaining);
        let target = self
            .effects
            .as_deref_mut()
            .ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?
            .resolve_reference_with_budget(reference, &mut budget);
        let debited = remaining.saturating_sub(budget.remaining());
        self.steps = self
            .steps
            .checked_add(debited)
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        let target = target?.ok_or_else(|| error("ORNA-EVAL-FIELD"))?;
        match Value::from_canonical(&target, self, depth + 1)? {
            Value::Record(fields) => fields
                .get(name)
                .cloned()
                .ok_or_else(|| error("ORNA-EVAL-FIELD")),
            Value::NominalRecord { type_id, fields } => {
                self.nominal_field(&type_id, &fields, name, scope)
            }
            _ => Err(error("ORNA-EVAL-TYPE")),
        }
    }

    fn numeric_postfix(&self, value: BigInt, name: &str) -> Result<Value, EvaluationError> {
        let unit = name.rsplit('.').next().unwrap_or(name);
        match unit {
            "decimal" => self
                .checked_decimal(DecimalValue::new(value, BigInt::zero())?)
                .map(Value::Decimal),
            "day" | "days" => Ok(Value::Period { days: value }),
            "hour" | "hours" => {
                let seconds = value
                    .checked_mul(&BigInt::from(3_600u32))
                    .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
                // Enforce the integer budget after conversion to the stored
                // seconds unit, as decimal duration postfixes do.
                Ok(Value::Duration {
                    seconds: self.integer(seconds)?,
                    nanosecond: 0,
                })
            }
            "minute" | "minutes" | "min" => {
                let seconds = value
                    .checked_mul(&BigInt::from(60u32))
                    .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
                Ok(Value::Duration {
                    seconds: self.integer(seconds)?,
                    nanosecond: 0,
                })
            }
            "second" | "seconds" | "s" => Ok(Value::Duration {
                seconds: self.integer(value)?,
                nanosecond: 0,
            }),
            _ => Err(error("ORNA-EVAL-TYPE")),
        }
    }
    fn numeric_decimal_postfix(
        &self,
        value: DecimalValue,
        name: &str,
    ) -> Result<Value, EvaluationError> {
        let unit = name.rsplit('.').next().unwrap_or(name);
        if unit == "decimal" {
            return self.checked_decimal(value).map(Value::Decimal);
        }
        let seconds_per_unit = match unit {
            "hour" | "hours" => 3_600u32,
            "minute" | "minutes" | "min" => 60u32,
            "second" | "seconds" | "s" => 1u32,
            _ => return Err(error("ORNA-EVAL-TYPE")),
        };
        self.integer(value.coefficient.clone())?;
        self.integer(value.exponent10.clone())?;

        // Decimal elapsed units are accepted only when their exact value lands
        // on the core Duration nanosecond grid. This avoids host rounding in
        // all four pinned formatters while preserving their fractional output.
        let numerator = value.coefficient * BigInt::from(seconds_per_unit);
        let nanosecond_shift = value.exponent10 + BigInt::from(9u8);
        let total_nanoseconds = if !nanosecond_shift.is_negative() {
            let shift = nanosecond_shift
                .to_u32()
                .filter(|shift| *shift <= DEFAULT_INTEGER_DIGITS as u32)
                .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
            numerator * BigInt::from(10u8).pow(shift)
        } else {
            let shift = (-nanosecond_shift)
                .to_u32()
                .filter(|shift| *shift <= DEFAULT_INTEGER_DIGITS as u32)
                .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
            let divisor = BigInt::from(10u8).pow(shift);
            let (quotient, remainder) = numerator.div_rem(&divisor);
            if !remainder.is_zero() {
                return Err(error("ORNA-EVAL-VALUE"));
            }
            quotient
        };
        // The configured digit limit applies to source integers and the
        // stored seconds value, not this temporary nanosecond scaling.
        let (seconds, nanosecond) =
            total_nanoseconds.div_mod_floor(&BigInt::from(1_000_000_000u32));
        let nanosecond = nanosecond
            .to_u32()
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        self.integer(seconds.clone())?;
        Ok(Value::Duration {
            seconds,
            nanosecond,
        })
    }
    fn integer(&self, value: BigInt) -> Result<BigInt, EvaluationError> {
        if value.to_str_radix(10).len() > self.limits.max_integer_digits {
            Err(error("ORNA-EVAL-LIMIT"))
        } else {
            Ok(value)
        }
    }
    fn evaluate(
        &mut self,
        expression: &Expr,
        scope: &mut Scope,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        // Once a block emits a transfer, every enclosing expression is only a
        // transport frame. Do not run siblings, effects, or later statements.
        if self.transfer.is_some() {
            return Ok(Value::Null);
        }
        self.step()?;
        self.depth(depth)?;
        let result = match expression {
            Expr::Name { text, .. } => scope
                .0
                .get(text)
                .cloned()
                .or_else(|| {
                    (!self.restrict_function_names && self.functions.contains_key(text)).then(
                        || Value::Function {
                            name: text.clone(),
                            nominal_definitions: scope.3.clone(),
                        },
                    )
                })
                .or_else(|| {
                    self.aliases
                        .and_then(|aliases| aliases.get(text))
                        .filter(|name| self.functions.contains_key(*name))
                        .cloned()
                        .map(|name| Value::Function {
                            name,
                            nominal_definitions: scope.3.clone(),
                        })
                })
                .or_else(|| {
                    let namespace = self.namespace.as_deref()?;
                    let qualified = format!("{namespace}.{text}");
                    self.functions
                        .contains_key(&qualified)
                        .then_some(Value::Function {
                            name: qualified,
                            nominal_definitions: scope.3.clone(),
                        })
                })
                .ok_or_else(|| error("ORNA-EVAL-NAME")),
            Expr::Lambda {
                parameters, body, ..
            } => {
                self.items(parameters.len())?;
                self.items(scope.0.len())?;
                let mut captured = scope.clone();
                captured.1.extend(captured.0.keys().cloned());
                Ok(Value::Closure(Arc::new(Closure {
                    parameters: parameters
                        .iter()
                        .map(|parameter| Parameter {
                            pattern: parameter.pattern.clone(),
                            annotation: parameter.annotation.clone(),
                            default: None,
                            span: parameter.span.clone(),
                        })
                        .collect(),
                    body: body.as_ref().clone(),
                    captured,
                    namespace: self.namespace.clone(),
                })))
            }
            Expr::Literal { text, kind, .. } => self.literal(text, *kind),
            Expr::InterpolatedString { segments, .. } => {
                self.interpolated_string(segments, scope, depth)
            }
            Expr::Group { inner, .. } => self.evaluate(inner, scope, depth + 1),
            Expr::Unary { op, rhs, .. } => {
                let value = self.evaluate(rhs, scope, depth + 1)?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                self.unary(op, value)
            }
            Expr::Range {
                lower,
                operator,
                upper,
                ..
            } => {
                let lower = lower
                    .as_deref()
                    .map(|value| self.evaluate(value, scope, depth + 1))
                    .transpose()?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                let upper = upper
                    .as_deref()
                    .map(|value| self.evaluate(value, scope, depth + 1))
                    .transpose()?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                self.ordered_range_optional(lower, upper, operator == "..=")
            }
            Expr::Binary { lhs, op, rhs, .. } => self.binary(op, lhs, rhs, scope, depth),
            Expr::List { elements, .. } => self.sequence(elements, scope, depth).map(Value::List),
            Expr::Tuple { elements, .. } => self.sequence(elements, scope, depth).map(Value::Tuple),
            Expr::Record { fields, .. } => {
                self.items(fields.len())?;
                let mut result = BTreeMap::new();
                for field in fields {
                    let value = self.evaluate(&field.value, scope, depth + 1)?;
                    if self.transfer.is_some() {
                        return Ok(Value::Null);
                    }
                    if result.insert(field.name.clone(), value).is_some() {
                        return Err(error("ORNA-EVAL-VALUE"));
                    }
                }
                Ok(Value::Record(result))
            }
            Expr::Nominal { path, fields, .. } => self.nominal(path, fields, scope, depth),
            Expr::Block {
                statements, tail, ..
            } => self.block(statements, tail.as_deref(), scope, depth),
            Expr::Call {
                callee, arguments, ..
            } => self.call(callee, arguments, None, scope, depth),
            // Generic arguments are checked statically and erased at runtime.
            // Executing the ordinary call path preserves the resolved source
            // function while retaining the same bounded argument evaluation.
            Expr::GenericCall {
                callee, arguments, ..
            } => self.call(callee, arguments, None, scope, depth),
            Expr::Index { base, index, .. } => {
                let base = self.evaluate(base, scope, depth + 1)?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                let index = self.evaluate(index, scope, depth + 1)?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                self.index(base, index)
            }
            Expr::Field { base, name, .. } => {
                let base = self.evaluate(base, scope, depth + 1)?;
                match base {
                    Value::Record(fields) => fields
                        .get(name)
                        .cloned()
                        .ok_or_else(|| error("ORNA-EVAL-FIELD")),
                    Value::NominalRecord { type_id, fields } => {
                        self.nominal_field(&type_id, &fields, name, scope)
                    }
                    Value::Reference(reference) => {
                        self.reference_field(&reference, name, scope, depth + 1)
                    }
                    Value::Error(failure) => self.error_field(&failure, name, depth + 1),
                    Value::Int(value) => self.numeric_postfix(value, name),
                    Value::Decimal(value) => self.numeric_decimal_postfix(value, name),
                    _ if self.transfer.is_some() => Ok(Value::Null),
                    _ => Err(error("ORNA-EVAL-TYPE")),
                }
            }
            Expr::Control {
                kind: ControlKind::If,
                condition: Some(condition),
                body: Some(body),
                alternate,
                ..
            } => match self.evaluate(condition, scope, depth + 1)? {
                _ if self.transfer.is_some() => Ok(Value::Null),
                Value::Bool(true) => self.evaluate(body, scope, depth + 1),
                Value::Bool(false) => alternate.as_deref().map_or(Ok(Value::Null), |value| {
                    self.evaluate(value, scope, depth + 1)
                }),
                _ => Err(error("ORNA-EVAL-TYPE")),
            },
            Expr::Control {
                kind: ControlKind::Case,
                condition: Some(condition),
                arms,
                ..
            } => self.case(condition, arms, scope, depth),
            Expr::Control {
                kind: ControlKind::While,
                binding: None,
                condition: Some(condition),
                body: Some(body),
                arms,
                alternate: None,
                ..
            } if arms.is_empty() => loop {
                match self.evaluate(condition, scope, depth + 1)? {
                    _ if self.transfer.is_some() => return Ok(Value::Null),
                    Value::Bool(false) => break Ok(Value::Unit),
                    Value::Bool(true) => {}
                    _ => break Err(error("ORNA-EVAL-TYPE")),
                }
                self.evaluate(body, scope, depth + 1)?;
                match self.transfer.take() {
                    None | Some(Transfer::Continue) => {}
                    Some(Transfer::Break(value)) => break Ok(value),
                    Some(transfer @ Transfer::Return(_)) => {
                        self.transfer = Some(transfer);
                        break Ok(Value::Null);
                    }
                }
            },
            Expr::Control {
                kind: ControlKind::Loop,
                binding: None,
                condition: None,
                body: Some(body),
                arms,
                alternate: None,
                ..
            } if arms.is_empty() => loop {
                self.evaluate(body, scope, depth + 1)?;
                match self.transfer.take() {
                    None | Some(Transfer::Continue) => {}
                    Some(Transfer::Break(value)) => break Ok(value),
                    Some(transfer @ Transfer::Return(_)) => {
                        self.transfer = Some(transfer);
                        break Ok(Value::Null);
                    }
                }
            },
            Expr::Control {
                kind: ControlKind::For,
                binding: Some(binding),
                condition: Some(iterable),
                body: Some(body),
                arms,
                alternate: None,
                ..
            } => {
                if arms.len() != 1
                    || arms[0].guard.is_some()
                    || arms[0].pattern != *binding
                    || arms[0].body != **body
                {
                    return Err(error("ORNA-EVAL-UNSUPPORTED"));
                }
                let iterable = self.evaluate(iterable, scope, depth + 1)?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                let values = self.finite_iterable(iterable)?;
                let outer_names = scope.0.keys().cloned().collect::<Vec<_>>();
                let bound_names = pattern_names(binding);
                for value in values {
                    let mut iteration = scope.clone();
                    if !bind(binding, value, &mut iteration, self, depth + 1)? {
                        return Err(error("ORNA-EVAL-TYPE"));
                    }
                    self.evaluate(body, &mut iteration, depth + 1)?;
                    match self.transfer.take() {
                        Some(Transfer::Continue) => {}
                        Some(Transfer::Break(value)) => {
                            for name in &outer_names {
                                if bound_names.contains(name) {
                                    continue;
                                }
                                if let Some(value) = iteration.0.get(name).cloned() {
                                    scope.0.insert(name.clone(), value);
                                }
                            }
                            return Ok(value);
                        }
                        Some(transfer @ Transfer::Return(_)) => {
                            self.transfer = Some(transfer);
                            return Ok(Value::Null);
                        }
                        None => {}
                    }
                    for name in &outer_names {
                        if bound_names.contains(name) {
                            continue;
                        }
                        if let Some(value) = iteration.0.get(name).cloned() {
                            scope.0.insert(name.clone(), value);
                        }
                    }
                }
                Ok(Value::Unit)
            }
            Expr::ReplBinding { text, .. } if self.repl_bindings => scope
                .0
                .get(text)
                .cloned()
                .ok_or_else(|| error("ORNA-EVAL-NAME")),
            Expr::ReplBinding { .. } => Err(error("ORNA-EVAL-UNSUPPORTED")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        };
        if self.transfer.is_some() {
            Ok(Value::Null)
        } else {
            result
        }
    }
    fn nominal(
        &mut self,
        path: &[orna_syntax_v1::NameSegment],
        supplied: &[orna_syntax_v1::RecordField],
        scope: &Scope,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(path.len().saturating_add(supplied.len()))?;
        if path.is_empty() {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        validate_nominal_definitions(&scope.3, self.limits)?;
        let spelling = path
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<Vec<_>>()
            .join(".");
        let qualified = (path.len() == 1)
            .then(|| {
                self.namespace
                    .as_deref()
                    .map(|namespace| format!("{namespace}.{spelling}"))
            })
            .flatten();
        let definition = scope
            .3
            .get(qualified.as_deref().unwrap_or_default())
            .or_else(|| scope.3.get(&spelling))
            .cloned()
            .ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?;
        self.items(definition.fields.len())?;

        let private_allowed = definition.owner.as_deref() == self.namespace.as_deref();
        let mut indexes = BTreeMap::new();
        for (index, field) in definition.fields.iter().enumerate() {
            if field.name.len() > self.limits.max_string_bytes
                || indexes.insert(field.name.clone(), index).is_some()
            {
                return Err(error("ORNA-EVAL-VALUE"));
            }
        }
        let mut supplied_names = BTreeSet::new();
        for field in supplied {
            let Some(&index) = indexes.get(&field.name) else {
                return Err(error("ORNA-EVAL-ARGUMENT"));
            };
            if !supplied_names.insert(field.name.clone()) {
                return Err(error("ORNA-EVAL-ARGUMENT"));
            }
            if !definition.fields[index].public && !private_allowed {
                return Err(error("ORNA-EVAL-UNSUPPORTED"));
            }
        }
        for field in &definition.fields {
            if supplied_names.contains(&field.name) {
                continue;
            }
            if field.default.is_none() {
                return Err(if field.public {
                    error("ORNA-EVAL-ARGUMENT")
                } else {
                    error("ORNA-EVAL-UNSUPPORTED")
                });
            }
        }

        let mut construction_scope = scope.clone();
        let mut values = BTreeMap::new();
        for field in supplied {
            let value = self.evaluate(&field.value, &mut construction_scope, depth + 1)?;
            if self.transfer.is_some() {
                return Ok(Value::Null);
            }
            construction_scope
                .0
                .insert(field.name.clone(), value.clone());
            values.insert(field.name.clone(), value);
        }
        for field in &definition.fields {
            if supplied_names.contains(&field.name) {
                continue;
            }
            let previous_namespace = self.namespace.clone();
            self.namespace = definition.owner.clone();
            let result = self.evaluate(
                field
                    .default
                    .as_ref()
                    .expect("omitted defaults were admitted above"),
                &mut construction_scope,
                depth + 1,
            );
            self.namespace = previous_namespace;
            let value = result?;
            if self.transfer.is_some() {
                return Ok(Value::Null);
            }
            construction_scope
                .0
                .insert(field.name.clone(), value.clone());
            values.insert(field.name.clone(), value);
        }
        let fields = definition
            .fields
            .into_iter()
            .map(|field| {
                let value = values
                    .remove(&field.name)
                    .expect("all admitted nominal fields are materialized");
                (field.field_id, value)
            })
            .collect::<Vec<_>>();
        Ok(Value::NominalRecord {
            type_id: definition.type_id,
            fields,
        })
    }
    fn nominal_field(
        &mut self,
        type_id: &Raw,
        fields: &[(Raw, Value)],
        name: &str,
        scope: &Scope,
    ) -> Result<Value, EvaluationError> {
        if !is_object_id_raw(type_id) {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        validate_nominal_definitions(&scope.3, self.limits)?;
        let mut definition = None;
        for candidate in scope.3.values() {
            if candidate.type_id == *type_id {
                if definition.is_some() {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                definition = Some(candidate);
            }
        }
        let Some(definition) = definition else {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        };
        let definition_field_ids = validate_nominal_definition(definition, self.limits)?;
        self.items(definition.fields.len())?;
        let mut field_ids = BTreeSet::new();
        for (key, _) in fields {
            let Some(field_id) = object_id_key(key) else {
                return Err(error("ORNA-EVAL-VALUE"));
            };
            if !field_ids.insert(field_id) {
                return Err(error("ORNA-EVAL-VALUE"));
            }
        }
        if field_ids != definition_field_ids {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        let Some(field) = definition.fields.iter().find(|field| field.name == name) else {
            return Err(error("ORNA-EVAL-FIELD"));
        };
        if !field.public && definition.owner.as_deref() != self.namespace.as_deref() {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        let selected = fields
            .iter()
            .find(|(key, _)| key == &field.field_id)
            .map(|(_, value)| value);
        selected.cloned().ok_or_else(|| error("ORNA-EVAL-FIELD"))
    }
    fn literal(&self, text: &str, kind: LiteralKind) -> Result<Value, EvaluationError> {
        match kind {
            LiteralKind::Null => Ok(Value::Null),
            LiteralKind::Boolean => Ok(Value::Bool(text == "true")),
            LiteralKind::String => self.string(unescape_string(text)?).map(Value::String),
            LiteralKind::Integer => parse_int(text)
                .and_then(|value| self.integer(value))
                .map(Value::Int),
            LiteralKind::Decimal => parse_decimal(text)
                .and_then(|value| {
                    self.integer(value.0)
                        .and_then(|coefficient| DecimalValue::new(coefficient, value.1))
                })
                .map(Value::Decimal),
            LiteralKind::Float => {
                let value = text
                    .strip_suffix('f')
                    .ok_or_else(|| error("ORNA-EVAL-VALUE"))?
                    .replace('_', "")
                    .parse::<f64>()
                    .map_err(|_| error("ORNA-EVAL-VALUE"))?;
                if value.is_finite() {
                    Ok(Value::Float(value.to_bits()))
                } else {
                    Err(error("ORNA-EVAL-VALUE"))
                }
            }
            LiteralKind::Date => {
                let value = text.to_owned();
                if !valid_date_literal(&value) {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                CanonicalValue::new(Raw::Tag(60001, Box::new(Raw::Text(value.clone()))))
                    .map_err(|_| error("ORNA-EVAL-VALUE"))?;
                self.string(value).map(Value::Date)
            }
            LiteralKind::Instant => {
                let (unix_seconds, nanosecond) =
                    parse_instant_literal(text).ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                CanonicalValue::new(Raw::Tag(
                    60002,
                    Box::new(Raw::Array(vec![
                        Raw::Int(unix_seconds.into()),
                        Raw::Int(nanosecond.into()),
                    ])),
                ))
                .map_err(|_| error("ORNA-EVAL-VALUE"))?;
                Ok(Value::Instant {
                    unix_seconds,
                    nanosecond,
                })
            }
        }
    }
    fn interpolated_string(
        &mut self,
        segments: &[StringSegment],
        scope: &mut Scope,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(segments.len())?;
        let mut output = String::new();
        for segment in segments {
            match segment {
                StringSegment::Text { text, .. } => output.push_str(&unescape_string_body(text)?),
                StringSegment::Expression { value, .. } => {
                    let Value::String(value) = self.evaluate(value, scope, depth + 1)? else {
                        return Err(error("ORNA-EVAL-TYPE"));
                    };
                    output.push_str(&value);
                }
            }
            if output.len() > self.limits.max_string_bytes {
                return Err(error("ORNA-EVAL-LIMIT"));
            }
        }
        Ok(Value::String(output))
    }
    fn sequence(
        &mut self,
        elements: &[Expr],
        scope: &mut Scope,
        depth: usize,
    ) -> Result<Vec<Value>, EvaluationError> {
        self.items(elements.len())?;
        let mut values = Vec::with_capacity(elements.len());
        for element in elements {
            values.push(self.evaluate(element, scope, depth + 1)?);
            if self.transfer.is_some() {
                return Ok(Vec::new());
            }
        }
        Ok(values)
    }
    fn block(
        &mut self,
        statements: &[Statement],
        tail: Option<&Expr>,
        scope: &mut Scope,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(statements.len())?;
        let outer_names = scope.0.keys().cloned().collect::<Vec<_>>();
        let mut declared_names = BTreeSet::new();
        let mut local = scope.clone();
        for statement in statements {
            match statement {
                Statement::Let { pattern, value, .. } => {
                    let value = self.evaluate(value, &mut local, depth + 1)?;
                    if !bind(pattern, value, &mut local, self, depth + 1)? {
                        return Err(error("ORNA-EVAL-TYPE"));
                    }
                    declared_names.extend(pattern_names(pattern));
                }
                Statement::Assert { value, .. } => {
                    match self.evaluate(value, &mut local, depth + 1)? {
                        Value::Bool(true) => {}
                        Value::Bool(false) => return Err(error("ORNA-EVAL-ASSERT")),
                        _ => return Err(error("ORNA-EVAL-TYPE")),
                    }
                }
                Statement::Assignment {
                    target,
                    operator,
                    value,
                    ..
                } => {
                    self.assignment(target, *operator, value, &mut local, depth + 1)?;
                }
                Statement::Expression { value, .. } => {
                    self.evaluate(value, &mut local, depth + 1)?;
                }
                Statement::Control { value, .. } => {
                    self.evaluate(value, &mut local, depth + 1)?;
                }
                Statement::Return { value, .. } => {
                    let value = value.as_ref().map_or(Ok(Value::Null), |value| {
                        self.evaluate(value, &mut local, depth + 1)
                    })?;
                    if self.transfer.is_none() {
                        self.transfer = Some(Transfer::Return(value));
                    }
                }
                Statement::Break { value, .. } => {
                    let value = value.as_ref().map_or(Ok(Value::Unit), |value| {
                        self.evaluate(value, &mut local, depth + 1)
                    })?;
                    if self.transfer.is_none() {
                        self.transfer = Some(Transfer::Break(value));
                    }
                }
                Statement::Continue { .. } => {
                    self.transfer = Some(Transfer::Continue);
                }
            }
            if self.transfer.is_some() {
                break;
            }
        }
        let result = if self.transfer.is_some() {
            Value::Null
        } else {
            tail.map_or(Ok(Value::Null), |value| {
                self.evaluate(value, &mut local, depth + 1)
            })?
        };
        for name in outer_names {
            if declared_names.contains(&name) {
                continue;
            }
            if let Some(value) = local.0.get(&name).cloned() {
                scope.0.insert(name, value);
            }
        }
        Ok(result)
    }
    fn assignment(
        &mut self,
        target: &AssignmentTarget,
        operator: AssignmentOperator,
        expression: &Expr,
        scope: &mut Scope,
        depth: usize,
    ) -> Result<(), EvaluationError> {
        let AssignmentTarget::Name { name, .. } = target else {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        };
        if scope.1.contains(name) {
            return Err(error("ORNA-EVAL-IMMUTABLE-CAPTURE"));
        }
        let current = scope
            .0
            .get(name)
            .cloned()
            .ok_or_else(|| error("ORNA-EVAL-NAME"))?;
        let value = self.evaluate(expression, scope, depth + 1)?;
        if self.transfer.is_some() {
            return Ok(());
        }
        let value = match operator {
            AssignmentOperator::Set => value,
            AssignmentOperator::Add => self.apply_binary("+", current, value)?,
            AssignmentOperator::Subtract => self.apply_binary("-", current, value)?,
            AssignmentOperator::Multiply => self.apply_binary("*", current, value)?,
            AssignmentOperator::Divide => self.apply_binary("/", current, value)?,
        };
        scope.0.insert(name.clone(), value);
        Ok(())
    }
    fn unary(&self, op: &str, value: Value) -> Result<Value, EvaluationError> {
        match (op, value) {
            ("!", Value::Bool(value)) => Ok(Value::Bool(!value)),
            (
                "+",
                value @ (Value::Int(_) | Value::Decimal(_) | Value::Money { .. } | Value::Float(_)),
            ) => Ok(value),
            ("-", Value::Int(value)) => self.integer(-value).map(Value::Int),
            ("-", Value::Decimal(value)) => self
                .checked_decimal(DecimalValue::new(-value.coefficient, value.exponent10)?)
                .map(Value::Decimal),
            ("-", Value::Money { amount, currency }) => self
                .checked_decimal(DecimalValue::new(-amount.coefficient, amount.exponent10)?)
                .map(|amount| Value::Money { amount, currency }),
            (
                "-",
                Value::Duration {
                    seconds,
                    nanosecond,
                },
            ) => self
                .duration_from_total_nanoseconds(-elapsed_total_nanoseconds(&seconds, nanosecond)),
            ("-", Value::Float(value)) => finite_float(-f64::from_bits(value)),
            _ => Err(error("ORNA-EVAL-TYPE")),
        }
    }
    fn binary(
        &mut self,
        op: &str,
        lhs: &Expr,
        rhs: &Expr,
        scope: &mut Scope,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        match op {
            ".." | "..=" => {
                let lower = self.evaluate(lhs, scope, depth + 1)?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                let upper = self.evaluate(rhs, scope, depth + 1)?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                self.ordered_range(lower, upper, op == "..=")
            }
            "in" => {
                let value = self.evaluate(lhs, scope, depth + 1)?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                let range = self.evaluate(rhs, scope, depth + 1)?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                self.range_contains(value, range)
            }
            "|" => {
                let input = match system_relation_source(lhs) {
                    Some(source) => Value::Relation(RelationPlan::new(source.to_owned())),
                    None => self.evaluate(lhs, scope, depth + 1)?,
                };
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                let (callee, arguments) = match rhs {
                    Expr::Call {
                        callee, arguments, ..
                    } => (callee.as_ref(), arguments.as_slice()),
                    _ => (rhs, &[][..]),
                };
                self.call(callee, arguments, Some(input), scope, depth)
            }
            "|?" => match self.evaluate(lhs, scope, depth + 1) {
                Ok(value) => Ok(value),
                Err(failure) => {
                    // Cancellation is a separate abrupt completion, not an
                    // Error delivered to a recovery handler.
                    if self
                        .cancellation
                        .is_some_and(CancellationToken::is_requested)
                    {
                        return Err(failure);
                    }
                    let (callee, arguments) = match rhs {
                        Expr::Call {
                            callee, arguments, ..
                        } => (callee.as_ref(), arguments.as_slice()),
                        _ => (rhs, &[][..]),
                    };
                    self.call(callee, arguments, Some(Value::Error(failure)), scope, depth)
                }
            },
            "??" => match self.evaluate(lhs, scope, depth + 1)? {
                _ if self.transfer.is_some() => Ok(Value::Null),
                Value::Option(Some(value)) => Ok(*value),
                Value::Option(None) | Value::Null => self.evaluate(rhs, scope, depth + 1),
                _ => Err(error("ORNA-EVAL-TYPE")),
            },
            "&&" => match self.evaluate(lhs, scope, depth + 1)? {
                _ if self.transfer.is_some() => Ok(Value::Null),
                Value::Bool(false) => Ok(Value::Bool(false)),
                Value::Bool(true) => match self.evaluate(rhs, scope, depth + 1)? {
                    Value::Bool(value) => Ok(Value::Bool(value)),
                    _ => Err(error("ORNA-EVAL-TYPE")),
                },
                _ => Err(error("ORNA-EVAL-TYPE")),
            },
            "||" => match self.evaluate(lhs, scope, depth + 1)? {
                _ if self.transfer.is_some() => Ok(Value::Null),
                Value::Bool(true) => Ok(Value::Bool(true)),
                Value::Bool(false) => match self.evaluate(rhs, scope, depth + 1)? {
                    Value::Bool(value) => Ok(Value::Bool(value)),
                    _ => Err(error("ORNA-EVAL-TYPE")),
                },
                _ => Err(error("ORNA-EVAL-TYPE")),
            },
            _ => {
                let left = self.evaluate(lhs, scope, depth + 1)?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                let right = self.evaluate(rhs, scope, depth + 1)?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                self.apply_binary(op, left, right)
            }
        }
    }
    fn apply_binary(&self, op: &str, left: Value, right: Value) -> Result<Value, EvaluationError> {
        if matches!(op, "==" | "!=") {
            if left.contains_callable() || right.contains_callable() {
                return Err(error("ORNA-EVAL-UNSUPPORTED"));
            }
            let equal = match (&left, &right) {
                (Value::Float(left), Value::Float(right)) => float_ordinary_eq(*left, *right),
                _ => left == right,
            };
            return Ok(Value::Bool(if op == "==" { equal } else { !equal }));
        }
        if matches!((&left, &right), (Value::Range { .. }, Value::Range { .. })) {
            return compare(op, compare_values(&left, &right)?);
        }
        match (left, right) {
            (Value::Int(a), Value::Int(b)) => self.int_binary(op, a, b),
            (Value::Decimal(a), Value::Decimal(b)) => self.decimal_binary(op, a, b),
            (
                Value::Instant {
                    unix_seconds,
                    nanosecond,
                },
                Value::Duration {
                    seconds,
                    nanosecond: duration_nanosecond,
                },
            ) if matches!(op, "+" | "-") => self.instant_duration_binary(
                unix_seconds,
                nanosecond,
                seconds,
                duration_nanosecond,
                op == "-",
            ),
            (
                Value::Duration {
                    seconds,
                    nanosecond: duration_nanosecond,
                },
                Value::Instant {
                    unix_seconds,
                    nanosecond,
                },
            ) if op == "+" => self.instant_duration_binary(
                unix_seconds,
                nanosecond,
                seconds,
                duration_nanosecond,
                false,
            ),
            (
                Value::Instant {
                    unix_seconds: left_seconds,
                    nanosecond: left_nanosecond,
                },
                Value::Instant {
                    unix_seconds: right_seconds,
                    nanosecond: right_nanosecond,
                },
            ) if op == "-" => self.duration_from_total_nanoseconds(
                elapsed_total_nanoseconds(&BigInt::from(left_seconds), left_nanosecond)
                    - elapsed_total_nanoseconds(&BigInt::from(right_seconds), right_nanosecond),
            ),
            (
                Value::Duration {
                    seconds: left_seconds,
                    nanosecond: left_nanosecond,
                },
                Value::Duration {
                    seconds: right_seconds,
                    nanosecond: right_nanosecond,
                },
            ) if matches!(op, "+" | "-") => {
                let left = elapsed_total_nanoseconds(&left_seconds, left_nanosecond);
                let right = elapsed_total_nanoseconds(&right_seconds, right_nanosecond);
                self.duration_from_total_nanoseconds(if op == "+" {
                    left + right
                } else {
                    left - right
                })
            }
            (
                Value::Duration {
                    seconds,
                    nanosecond,
                },
                Value::Int(factor),
            ) if op == "*" => self.duration_from_total_nanoseconds(
                elapsed_total_nanoseconds(&seconds, nanosecond) * factor,
            ),
            (
                Value::Int(factor),
                Value::Duration {
                    seconds,
                    nanosecond,
                },
            ) if op == "*" => self.duration_from_total_nanoseconds(
                factor * elapsed_total_nanoseconds(&seconds, nanosecond),
            ),
            (
                Value::Duration {
                    seconds,
                    nanosecond,
                },
                Value::Int(divisor),
            ) if op == "/" => {
                if divisor.is_zero() {
                    return Err(error("ORNA-EVAL-DIVIDE-BY-ZERO"));
                }
                let total = elapsed_total_nanoseconds(&seconds, nanosecond);
                let (quotient, remainder) = total.div_rem(&divisor);
                // Orna does not prescribe Duration scaling. Keep it exact on
                // the stored nanosecond grid instead of silently rounding.
                if !remainder.is_zero() {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                self.duration_from_total_nanoseconds(quotient)
            }
            (
                Value::Duration {
                    seconds,
                    nanosecond,
                },
                Value::Decimal(scalar),
            ) if op == "*" => self.duration_decimal_scale(
                elapsed_total_nanoseconds(&seconds, nanosecond),
                scalar,
                false,
            ),
            (
                Value::Decimal(scalar),
                Value::Duration {
                    seconds,
                    nanosecond,
                },
            ) if op == "*" => self.duration_decimal_scale(
                elapsed_total_nanoseconds(&seconds, nanosecond),
                scalar,
                false,
            ),
            (
                Value::Duration {
                    seconds,
                    nanosecond,
                },
                Value::Decimal(scalar),
            ) if op == "/" => self.duration_decimal_scale(
                elapsed_total_nanoseconds(&seconds, nanosecond),
                scalar,
                true,
            ),
            (
                Value::Money {
                    amount: a,
                    currency: ac,
                },
                Value::Money {
                    amount: b,
                    currency: bc,
                },
            ) => self.money_binary(op, a, ac, b, bc),
            (Value::Money { amount, currency }, Value::Decimal(scalar)) => {
                self.money_scalar_binary(op, amount, currency, scalar, false)
            }
            (Value::Decimal(scalar), Value::Money { amount, currency }) => {
                self.money_scalar_binary(op, amount, currency, scalar, true)
            }
            (Value::Float(a), Value::Float(b)) => self.float_binary(op, a, b),
            (Value::String(a), Value::String(b)) => compare(op, a.cmp(&b)),
            (Value::Date(a), Value::Date(b)) => compare(op, a.cmp(&b)),
            (
                Value::Instant {
                    unix_seconds: a_seconds,
                    nanosecond: a_nanosecond,
                },
                Value::Instant {
                    unix_seconds: b_seconds,
                    nanosecond: b_nanosecond,
                },
            ) => compare(
                op,
                a_seconds
                    .cmp(&b_seconds)
                    .then(a_nanosecond.cmp(&b_nanosecond)),
            ),
            (
                Value::Duration {
                    seconds: a_seconds,
                    nanosecond: a_nanosecond,
                },
                Value::Duration {
                    seconds: b_seconds,
                    nanosecond: b_nanosecond,
                },
            ) => compare(
                op,
                a_seconds
                    .cmp(&b_seconds)
                    .then(a_nanosecond.cmp(&b_nanosecond)),
            ),
            (Value::Bool(a), Value::Bool(b)) => compare(op, a.cmp(&b)),
            _ => Err(error("ORNA-EVAL-TYPE")),
        }
    }
    fn duration_from_total_nanoseconds(
        &self,
        total_nanoseconds: BigInt,
    ) -> Result<Value, EvaluationError> {
        // Normalize on the stored Duration grid so negative values keep a
        // floor-normalized seconds field and a nonnegative nanosecond tail.
        let (seconds, nanosecond) =
            total_nanoseconds.div_mod_floor(&BigInt::from(1_000_000_000u32));
        let nanosecond = nanosecond
            .to_u32()
            .filter(|nanosecond| *nanosecond < 1_000_000_000)
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        Ok(Value::Duration {
            seconds: self.integer(seconds)?,
            nanosecond,
        })
    }
    fn duration_decimal_scale(
        &self,
        total_nanoseconds: BigInt,
        scalar: DecimalValue,
        divide: bool,
    ) -> Result<Value, EvaluationError> {
        self.integer(scalar.coefficient.clone())?;
        self.integer(scalar.exponent10.clone())?;
        if divide && scalar.coefficient.is_zero() {
            return Err(error("ORNA-EVAL-DIVIDE-BY-ZERO"));
        }

        let mut numerator = if divide {
            total_nanoseconds
        } else {
            total_nanoseconds * &scalar.coefficient
        };
        let mut denominator = if divide {
            scalar.coefficient
        } else {
            BigInt::from(1u8)
        };
        let decimal_shift = if divide {
            -scalar.exponent10
        } else {
            scalar.exponent10
        };
        if decimal_shift.is_negative() {
            let power = (-decimal_shift)
                .to_u32()
                .filter(|power| *power <= DEFAULT_INTEGER_DIGITS as u32)
                .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
            denominator *= BigInt::from(10u8).pow(power);
        } else {
            let power = decimal_shift
                .to_u32()
                .filter(|power| *power <= DEFAULT_INTEGER_DIGITS as u32)
                .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
            numerator *= BigInt::from(10u8).pow(power);
        }

        let (quotient, remainder) = numerator.div_rem(&denominator);
        // Decimal scaling may be rational, but Duration storage has a one
        // nanosecond quantum; reject a remainder instead of rounding it away.
        if !remainder.is_zero() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        self.duration_from_total_nanoseconds(quotient)
    }
    fn instant_duration_binary(
        &self,
        instant_seconds: i64,
        instant_nanosecond: u32,
        duration_seconds: BigInt,
        duration_nanosecond: u32,
        subtract: bool,
    ) -> Result<Value, EvaluationError> {
        let instant = elapsed_total_nanoseconds(&BigInt::from(instant_seconds), instant_nanosecond);
        let duration = elapsed_total_nanoseconds(&duration_seconds, duration_nanosecond);
        let total = if subtract {
            instant - duration
        } else {
            instant + duration
        };
        let (seconds, nanosecond) = total.div_mod_floor(&BigInt::from(1_000_000_000u32));
        // Instant has a fixed i64 seconds axis; out-of-range arithmetic is a
        // value error rather than wrapping or borrowing calendar policy.
        let unix_seconds = seconds.to_i64().ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
        let nanosecond = nanosecond
            .to_u32()
            .filter(|nanosecond| *nanosecond < 1_000_000_000)
            .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
        Ok(Value::Instant {
            unix_seconds,
            nanosecond,
        })
    }
    fn ordered_range(
        &self,
        lower: Value,
        upper: Value,
        upper_inclusive: bool,
    ) -> Result<Value, EvaluationError> {
        self.ordered_range_optional(Some(lower), Some(upper), upper_inclusive)
    }
    fn ordered_range_optional(
        &self,
        lower: Option<Value>,
        upper: Option<Value>,
        upper_inclusive: bool,
    ) -> Result<Value, EvaluationError> {
        validate_range_endpoints(lower.as_ref(), upper.as_ref())?;
        Ok(Value::Range {
            lower: lower.map(Box::new),
            upper: upper.map(Box::new),
            upper_inclusive,
        })
    }
    fn range_contains(&self, value: Value, range: Value) -> Result<Value, EvaluationError> {
        let Value::Range {
            lower,
            upper,
            upper_inclusive,
        } = range
        else {
            return Err(error("ORNA-EVAL-TYPE"));
        };
        let range_kind = range_endpoint_kind_for_range(lower.as_deref(), upper.as_deref())?;
        if range_endpoint_kind(&value) != Some(range_kind) {
            return Err(error("ORNA-EVAL-TYPE"));
        }
        let lower_matches = match lower {
            Some(lower) => {
                range_membership_compare(&value, &lower)?.is_some_and(|ordering| ordering.is_ge())
            }
            None => true,
        };
        let upper_matches = match upper {
            Some(upper) => range_membership_compare(&value, &upper)?.is_some_and(|ordering| {
                if upper_inclusive {
                    ordering.is_le()
                } else {
                    ordering.is_lt()
                }
            }),
            None => true,
        };
        Ok(Value::Bool(lower_matches && upper_matches))
    }
    fn finite_iterable(&self, value: Value) -> Result<Vec<Value>, EvaluationError> {
        match value {
            Value::List(values) => {
                self.items(values.len())?;
                Ok(values)
            }
            Value::Range {
                lower: Some(lower),
                upper: Some(upper),
                upper_inclusive,
            } => match (*lower, *upper) {
                (Value::Int(lower), Value::Int(upper)) => {
                    self.integer_range_values(lower, upper, upper_inclusive)
                }
                _ => Err(error("ORNA-EVAL-TYPE")),
            },
            // A Range with an unbounded side is a valid Range value but not a
            // finite Iterable, so this bounded evaluator cannot consume it.
            Value::Range { .. } => Err(error("ORNA-EVAL-TYPE")),
            _ => Err(error("ORNA-EVAL-TYPE")),
        }
    }
    fn integer_range_values(
        &self,
        lower: BigInt,
        upper: BigInt,
        upper_inclusive: bool,
    ) -> Result<Vec<Value>, EvaluationError> {
        let count = if upper_inclusive {
            &upper - &lower + 1
        } else {
            &upper - &lower
        };
        if count <= BigInt::zero() {
            return Ok(Vec::new());
        }
        let count = count.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        self.items(count)?;
        let mut values = Vec::with_capacity(count);
        let mut value = lower;
        for _ in 0..count {
            values.push(Value::Int(value.clone()));
            value += 1;
        }
        Ok(values)
    }
    fn int_binary(&self, op: &str, a: BigInt, b: BigInt) -> Result<Value, EvaluationError> {
        match op {
            "+" => self.integer(a + b).map(Value::Int),
            "-" => self.integer(a - b).map(Value::Int),
            "*" => self.integer(a * b).map(Value::Int),
            "/" => {
                if b.is_zero() {
                    Err(error("ORNA-EVAL-DIVIDE-BY-ZERO"))
                } else {
                    self.integer(a / b).map(Value::Int)
                }
            }
            "%" => {
                if b.is_zero() {
                    Err(error("ORNA-EVAL-DIVIDE-BY-ZERO"))
                } else {
                    self.integer(a % b).map(Value::Int)
                }
            }
            "^" => self.integer_power(a, b).map(Value::Int),
            "<" | "<=" | ">" | ">=" => compare(op, a.cmp(&b)),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn integer_power(
        &self,
        mut base: BigInt,
        mut exponent: BigInt,
    ) -> Result<BigInt, EvaluationError> {
        // Orna 1.0 specifies right-associative exponent precedence, but not
        // numeric edge cases. This evaluator chooses nonnegative integer
        // exponents; negative exponents return a type error because their
        // results are not integers. Check each multiplication against the
        // normal integer digit budget, avoiding unbounded intermediate growth.
        if exponent.is_negative() {
            return Err(error("ORNA-EVAL-TYPE"));
        }
        let mut result = BigInt::from(1u8);
        while !exponent.is_zero() {
            if exponent.is_odd() {
                result = self.integer(result * base.clone())?;
            }
            exponent /= 2u8;
            if !exponent.is_zero() {
                base = self.integer(base.clone() * base)?;
            }
        }
        Ok(result)
    }
    fn checked_decimal(&self, value: DecimalValue) -> Result<DecimalValue, EvaluationError> {
        self.integer(value.coefficient.clone())?;
        self.integer(value.exponent10.clone())?;
        Ok(value)
    }
    fn money_binary(
        &self,
        op: &str,
        left: DecimalValue,
        left_currency: [u8; 16],
        right: DecimalValue,
        right_currency: [u8; 16],
    ) -> Result<Value, EvaluationError> {
        if left_currency != right_currency {
            return Err(error("ORNA-EVAL-TYPE"));
        }
        match op {
            "+" => self
                .checked_decimal(left.add(&right)?)
                .map(|amount| Value::Money {
                    amount,
                    currency: left_currency,
                }),
            "-" => self
                .checked_decimal(
                    left.add(&DecimalValue::new(-right.coefficient, right.exponent10)?)?,
                )
                .map(|amount| Value::Money {
                    amount,
                    currency: left_currency,
                }),
            "<" | "<=" | ">" | ">=" => compare(
                op,
                compare_values(&Value::Decimal(left), &Value::Decimal(right))?,
            ),
            _ => Err(error("ORNA-EVAL-TYPE")),
        }
    }
    fn money_scalar_binary(
        &self,
        op: &str,
        amount: DecimalValue,
        currency: [u8; 16],
        scalar: DecimalValue,
        scalar_left: bool,
    ) -> Result<Value, EvaluationError> {
        let result = match (op, scalar_left) {
            ("*", _) => amount.multiply(&scalar),
            ("/", false) => amount.divide(&scalar),
            _ => return Err(error("ORNA-EVAL-TYPE")),
        }?;
        self.checked_decimal(result)
            .map(|amount| Value::Money { amount, currency })
    }
    fn decimal_binary(
        &self,
        op: &str,
        a: DecimalValue,
        b: DecimalValue,
    ) -> Result<Value, EvaluationError> {
        match op {
            "+" => a.add(&b),
            "-" => a.add(&DecimalValue::new(-b.coefficient, b.exponent10)?),
            "*" => a.multiply(&b),
            "/" => a.divide(&b),
            _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
        .map(Value::Decimal)
    }
    fn float_binary(&self, op: &str, a: u64, b: u64) -> Result<Value, EvaluationError> {
        let (a, b) = (f64::from_bits(a), f64::from_bits(b));
        match op {
            "+" => finite_float(a + b),
            "-" => finite_float(a - b),
            "*" => finite_float(a * b),
            "/" if b == 0.0 => Err(error("ORNA-EVAL-DIVIDE-BY-ZERO")),
            "/" => finite_float(a / b),
            "%" if b == 0.0 => Err(error("ORNA-EVAL-DIVIDE-BY-ZERO")),
            "%" => finite_float(a % b),
            "^" => {
                // Pragmatic real-float semantics use exp(exponent * ln(base)).
                // Negative bases are outside that domain; 0^0 is chosen as 1,
                // and zero to a negative power uses the division-by-zero code.
                if a == 0.0 {
                    if b < 0.0 {
                        Err(error("ORNA-EVAL-DIVIDE-BY-ZERO"))
                    } else if b == 0.0 {
                        finite_float(1.0)
                    } else {
                        finite_float(0.0)
                    }
                } else if a < 0.0 {
                    Err(error("ORNA-EVAL-TYPE"))
                } else {
                    finite_float((b * a.ln()).exp())
                }
            }
            "<" => Ok(Value::Bool(a < b)),
            "<=" => Ok(Value::Bool(a <= b)),
            ">" => Ok(Value::Bool(a > b)),
            ">=" => Ok(Value::Bool(a >= b)),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }

    fn relation_call(
        &mut self,
        callee: &Expr,
        arguments: &[orna_syntax_v1::Argument],
        input: Option<Value>,
        scope: &mut Scope,
        depth: usize,
    ) -> Option<Result<Value, EvaluationError>> {
        let resolved = self.resolve_function_name(callee, scope);
        let statistics_operation =
            portable_statistics_operation(callee, resolved.as_deref(), scope);
        let native_export = is_native_collection_binding(
            callee,
            resolved.as_deref(),
            scope,
            !self.restrict_function_names,
            function_name(callee).is_some_and(|name| self.functions.contains_key(&name)),
        ) || resolved
            .as_deref()
            .is_some_and(|name| matches!(name, "std.collection.asof_join" | "std.query.asof_join"));
        let intrinsic_name = root_collection_name(callee);
        let name = intrinsic_name
            .or_else(|| {
                native_export
                    .then(|| portable_collection_operation(callee, resolved.as_deref()))
                    .flatten()
            })
            .or(statistics_operation)?;
        if (statistics_operation.is_none()
            && intrinsic_name.is_some_and(|name| scope.0.contains_key(name)))
            || (resolved.is_some() && !native_export && statistics_operation.is_none())
        {
            return None;
        }
        let pipeline_relation = matches!(input, Some(Value::Relation(_)));
        let relation_argument = relation_call_candidate(name, arguments, scope);
        if input.is_some() && !pipeline_relation && !relation_argument {
            return None;
        }
        if input.is_none() && !relation_argument {
            return None;
        }

        Some((|| {
            let implicit = usize::from(input.is_some());
            let mut values = input.into_iter().collect::<Vec<_>>();
            for argument in arguments {
                let value = self.evaluate(&argument.value, scope, depth + 1)?;
                values.push(value);
            }
            let mut ordered = if statistics_operation.is_some() {
                relation_statistics_arguments(name, arguments, values, implicit)?
            } else {
                relation_named_arguments(name, arguments, values, implicit)?
            };
            if statistics_operation.is_some() {
                let Some(Value::Relation(plan)) = ordered.first() else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                let rows = self.collect_relation_values(plan, depth + 1)?;
                ordered[0] = Value::List(rows);
                return self.stats(name, ordered);
            }
            if name == "union" {
                let mut union_operands = ordered.into_iter();
                let (Some(left), Some(right)) = (union_operands.next(), union_operands.next())
                else {
                    return Err(error("ORNA-EVAL-ARGUMENT"));
                };
                let (Value::Relation(left), Value::Relation(right)) = (left, right) else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                return Ok(Value::Relation(RelationPlan::union(left, right)));
            }
            if matches!(
                name,
                "chunk"
                    | "flatten"
                    | "partition"
                    | "zip"
                    | "zip_exact"
                    | "group_by"
                    | "split_when"
                    | "rank"
                    | "asof_join"
            ) {
                return self.collection_relation_operation(name, ordered, depth);
            }
            if name == "filter" {
                let mut filter_arguments = ordered.into_iter();
                let Some(Value::Relation(plan)) = filter_arguments.next() else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                let Some(predicate) = filter_arguments.next() else {
                    return Err(error("ORNA-EVAL-ARGUMENT"));
                };
                // Filter compilation extends the owned plan directly. A
                // chain of filters over a nested union storm should copy the
                // tree only when the cascade is split between union children,
                // not once for every stage added to that tree.
                return Ok(Value::Relation(
                    plan.with_stage(RelationStage::Filter(vec![predicate])),
                ));
            }
            let Value::Relation(mut plan) = ordered[0].clone() else {
                return Err(error("ORNA-EVAL-TYPE"));
            };

            match name {
                // The reference specifies relation composition and order but
                // no separate projection grammar. `project` is the named
                // result-shaping spelling of the same lazy one-to-one map.
                "map" | "project" => {
                    plan = plan.with_stage(RelationStage::Map(ordered[1].clone()));
                    Ok(Value::Relation(plan))
                }
                "flat_map" => {
                    plan = plan.with_stage(RelationStage::FlatMap(ordered[1].clone()));
                    Ok(Value::Relation(plan))
                }
                "sort_by" => {
                    plan = plan.with_stage(RelationStage::SortBy(ordered[1].clone()));
                    Ok(Value::Relation(plan))
                }
                "bucket_by" => {
                    let spec = bucket_by_spec(&ordered[1], ordered.get(2))?;
                    plan = plan.with_stage(RelationStage::BucketBy(spec));
                    Ok(Value::Relation(plan))
                }
                "window" => {
                    let size = relation_window_argument(&ordered[1])?;
                    let step = ordered.get(2).map_or(Ok(1), relation_window_argument)?;
                    plan = plan.with_stage(RelationStage::Window(size, step));
                    Ok(Value::Relation(plan))
                }
                "distinct" => {
                    plan = plan.with_stage(RelationStage::Distinct);
                    Ok(Value::Relation(plan))
                }
                "unique" => {
                    plan = plan.with_stage(RelationStage::Distinct);
                    Ok(Value::Relation(plan))
                }
                "pairs" => {
                    plan = plan.with_stage(RelationStage::Pairs);
                    Ok(Value::Relation(plan))
                }
                "take" | "drop" => {
                    let Value::Int(count) = &ordered[1] else {
                        return Err(error("ORNA-EVAL-TYPE"));
                    };
                    if count.sign() == num_bigint::Sign::Minus {
                        return Err(error("ORNA-EVAL-VALUE"));
                    }
                    let count = count.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
                    plan = plan.with_stage(if name == "take" {
                        RelationStage::Take(count)
                    } else {
                        RelationStage::Drop(count)
                    });
                    Ok(Value::Relation(plan))
                }
                "count" | "first" | "last" | "one" | "every" | "exists" | "sum" | "min" | "max" => {
                    // Keep adjacent filters compiled as one cascade until
                    // terminal demand begins, then push the batch once.
                    let plan = plan.flush_filter_cascade();
                    self.observe_relation(&plan, name, &ordered[1..], depth)
                }
                _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
            }
        })())
    }

    fn collection_relation_operation(
        &mut self,
        name: &str,
        mut values: Vec<Value>,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        let collection_count = match name {
            "zip" | "zip_exact" | "asof_join" => 2,
            _ => 1,
        };
        if values.len() < collection_count {
            return Err(error("ORNA-EVAL-ARGUMENT"));
        }

        let mut relation_kinds = Vec::with_capacity(collection_count);
        for value in values.iter_mut().take(collection_count) {
            match value {
                Value::List(_) => relation_kinds.push(false),
                Value::Relation(plan) => {
                    relation_kinds.push(true);
                    *value = Value::List(self.collect_relation_values(plan, depth + 1)?);
                }
                _ => return Err(error("ORNA-EVAL-TYPE")),
            }
        }

        let relation_result = match name {
            "zip" | "zip_exact" => {
                if relation_kinds[0] != relation_kinds[1] {
                    return Err(error("ORNA-EVAL-TYPE"));
                }
                relation_kinds[0]
            }
            // The result follows the left operand's container kind; the right
            // side only supplies candidate rows to the as-of selector.
            "asof_join" => relation_kinds[0],
            _ => relation_kinds[0],
        };

        let result = self.collection(name, values, depth)?;
        if !relation_result {
            return Ok(result);
        }
        match result {
            Value::List(rows) => Ok(Value::Relation(RelationPlan::from_values(rows))),
            // `partition` has a scalar tuple result whose two components each
            // preserve the source container kind.
            Value::Tuple(parts) if name == "partition" && parts.len() == 2 => {
                let mut relations = Vec::with_capacity(2);
                for part in parts {
                    let Value::List(rows) = part else {
                        return Err(error("ORNA-EVAL-VALUE"));
                    };
                    relations.push(Value::Relation(RelationPlan::from_values(rows)));
                }
                Ok(Value::Tuple(relations))
            }
            _ => Err(error("ORNA-EVAL-VALUE")),
        }
    }

    fn for_each_bucket_group(
        &mut self,
        plan: &RelationPlan,
        bucket_index: usize,
        depth: usize,
        mut visit: impl FnMut(&mut Self, Value) -> Result<bool, EvaluationError>,
    ) -> Result<(), EvaluationError> {
        self.for_each_bucket_group_inner(plan, bucket_index, depth, &mut visit)
    }

    fn for_each_bucket_group_inner(
        &mut self,
        plan: &RelationPlan,
        bucket_index: usize,
        depth: usize,
        visit: &mut dyn FnMut(&mut Self, Value) -> Result<bool, EvaluationError>,
    ) -> Result<(), EvaluationError> {
        let RelationStage::BucketBy(spec) = &plan.stages[bucket_index] else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        let mut state = RelationBucketState::try_new(spec.clone()).map_err(bucket_error)?;
        let prefix = RelationPlan {
            source: plan.source.clone(),
            source_identity: plan.source_identity,
            source_union: plan.source_union.clone(),
            source_values: plan.source_values.clone(),
            stages: plan.stages[..bucket_index].to_vec(),
        };
        let suffix = &plan.stages[bucket_index + 1..];
        if let Some(sort_pos) = suffix
            .iter()
            .position(|stage| matches!(stage, RelationStage::SortBy(_)))
        {
            let RelationStage::SortBy(key) = &suffix[sort_pos] else {
                unreachable!("sort stage index");
            };
            let group_plan = RelationPlan {
                source: plan.source.clone(),
                source_identity: plan.source_identity,
                source_union: plan.source_union.clone(),
                source_values: plan.source_values.clone(),
                stages: plan.stages[..bucket_index + 1 + sort_pos].to_vec(),
            };
            let mut groups = Vec::new();
            let mut collect = |context: &mut Self, value: Value| {
                groups.push(value);
                context.items(groups.len())?;
                Ok(true)
            };
            self.for_each_bucket_group_inner(&group_plan, bucket_index, depth, &mut collect)?;
            let Value::List(sorted) =
                self.collection("sort_by", vec![Value::List(groups), key.clone()], depth)?
            else {
                unreachable!("sort_by returns a list");
            };
            let mut distinct_seen = vec![HashSet::new(); plan.stages.len()];
            let mut pair_previous = vec![None; plan.stages.len()];
            let mut window_states = (0..plan.stages.len())
                .map(|_| None)
                .collect::<Vec<Option<RelationWindowState>>>();
            return self.for_each_buffered_relation(
                sorted,
                &suffix[sort_pos + 1..],
                bucket_index + sort_pos + 2,
                depth,
                &mut distinct_seen,
                &mut pair_previous,
                &mut window_states,
                visit,
            );
        }
        let mut counters = vec![0usize; plan.stages.len()];
        let mut distinct_seen = vec![HashSet::new(); plan.stages.len()];
        let mut pair_previous = vec![None; plan.stages.len()];
        let mut window_states = (0..plan.stages.len())
            .map(|_| None)
            .collect::<Vec<Option<RelationWindowState>>>();
        let mut emit =
            |context: &mut Self, bucket: RelationBucket| -> Result<bool, EvaluationError> {
                let rows = context.apply_relation_stages(
                    Value::List(bucket.values),
                    suffix,
                    &mut counters,
                    &mut distinct_seen,
                    &mut pair_previous,
                    &mut window_states,
                    bucket_index + 1,
                    depth + 1,
                )?;
                for row in rows {
                    match row {
                        RelationRow::Skip => {}
                        RelationRow::End => return Ok(false),
                        RelationRow::Yield(value) => {
                            if !visit(context, value)? {
                                return Ok(false);
                            }
                        }
                    }
                }
                Ok(true)
            };
        let mut ended = false;
        self.for_each_sorted_relation(&prefix, depth, |context, value| {
            let flushed = state.push(value).map_err(bucket_error)?;
            if let Some(bucket) = flushed {
                if !emit(context, bucket)? {
                    ended = true;
                    return Ok(false);
                }
            }
            Ok(true)
        })?;
        if !ended {
            if let Some(bucket) = state.finish() {
                emit(self, bucket)?;
            }
        }
        Ok(())
    }

    fn observe_bucket_relation(
        &mut self,
        plan: &RelationPlan,
        bucket_index: usize,
        operation: &str,
        arguments: &[Value],
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        match operation {
            "count" => {
                let mut count = 0usize;
                self.for_each_bucket_group(plan, bucket_index, depth, |context, _| {
                    count = count
                        .checked_add(1)
                        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
                    context.items(count)?;
                    Ok(true)
                })?;
                Ok(Value::Int(BigInt::from(count)))
            }
            "first" => {
                let mut first = None;
                self.for_each_bucket_group(plan, bucket_index, depth, |_, value| {
                    first = Some(value);
                    Ok(false)
                })?;
                Ok(Value::Option(first.map(Box::new)))
            }
            "last" => {
                let mut state = RelationLastState::new();
                self.for_each_bucket_group(plan, bucket_index, depth, |_, value| {
                    state.push(value);
                    Ok(true)
                })?;
                Ok(Value::Option(state.finish().map(Box::new)))
            }
            "one" => {
                let mut found = None;
                self.for_each_bucket_group(plan, bucket_index, depth, |_, value| {
                    if found.is_some() {
                        return Err(error("ORNA-EVAL-RELATION-ONE-MULTIPLE"));
                    }
                    found = Some(value);
                    Ok(true)
                })?;
                found.ok_or_else(|| error("ORNA-EVAL-RELATION-ONE-ZERO"))
            }
            "sum" | "min" | "max" | "window" => {
                let mut values = Vec::new();
                self.for_each_bucket_group(plan, bucket_index, depth, |context, value| {
                    values.push(value);
                    context.items(values.len())?;
                    Ok(true)
                })?;
                self.observe_list_relation(operation, values, arguments, depth)
            }
            "every" | "exists" => {
                let predicate = arguments
                    .first()
                    .ok_or_else(|| error("ORNA-EVAL-ARGUMENT"))?;
                let want_exists = operation == "exists";
                let mut result = !want_exists;
                self.for_each_bucket_group(plan, bucket_index, depth, |context, value| {
                    context.step()?;
                    let Value::Bool(value) =
                        context.invoke_predicate(predicate, value, depth + 1)?
                    else {
                        return Err(error("ORNA-EVAL-TYPE"));
                    };
                    if value == want_exists {
                        result = value;
                        return Ok(false);
                    }
                    Ok(true)
                })?;
                Ok(Value::Bool(result))
            }
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }

    fn observe_relation(
        &mut self,
        plan: &RelationPlan,
        operation: &str,
        arguments: &[Value],
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        if let Some(bucket_index) = plan
            .stages
            .iter()
            .position(|stage| matches!(stage, RelationStage::BucketBy(_)))
        {
            return self.observe_bucket_relation(plan, bucket_index, operation, arguments, depth);
        }
        if plan
            .stages
            .iter()
            .any(|stage| matches!(stage, RelationStage::SortBy(_)))
        {
            return self.observe_sorted_relation(plan, operation, arguments, depth);
        }

        match operation {
            "last" => {
                let mut state = RelationLastState::new();
                self.for_each_relation_value(plan, depth, |_, value| {
                    state.push(value);
                    Ok(true)
                })?;
                Ok(Value::Option(state.finish().map(Box::new)))
            }
            "first" => {
                let mut first = None;
                self.for_each_relation_value(plan, depth, |_, value| {
                    first = Some(value);
                    Ok(false)
                })?;
                Ok(Value::Option(first.map(Box::new)))
            }
            "one" => {
                let predicate = arguments.first();
                let mut found = None;
                self.for_each_relation_value(plan, depth, |context, value| {
                    if let Some(predicate) = predicate {
                        context.step()?;
                        match context.invoke_predicate(predicate, value.clone(), depth + 1)? {
                            Value::Bool(true) => {}
                            Value::Bool(false) => return Ok(true),
                            _ => return Err(error("ORNA-EVAL-TYPE")),
                        }
                    }
                    if found.is_some() {
                        return Err(error("ORNA-EVAL-RELATION-ONE-MULTIPLE"));
                    }
                    found = Some(value);
                    Ok(true)
                })?;
                found.ok_or_else(|| error("ORNA-EVAL-RELATION-ONE-ZERO"))
            }
            "count" => {
                let mut count = 0usize;
                self.for_each_relation_value(plan, depth, |context, _| {
                    count = count
                        .checked_add(1)
                        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
                    context.items(count)?;
                    Ok(true)
                })?;
                Ok(Value::Int(BigInt::from(count)))
            }
            "every" | "exists" => {
                let predicate = arguments
                    .first()
                    .ok_or_else(|| error("ORNA-EVAL-ARGUMENT"))?;
                let want_exists = operation == "exists";
                let mut result = !want_exists;
                self.for_each_relation_value(plan, depth, |context, value| {
                    context.step()?;
                    let value = context.invoke_predicate(predicate, value, depth + 1)?;
                    let Value::Bool(value) = value else {
                        return Err(error("ORNA-EVAL-TYPE"));
                    };
                    if value == want_exists {
                        result = value;
                        return Ok(false);
                    }
                    Ok(true)
                })?;
                Ok(Value::Bool(result))
            }
            "window" => {
                let values = self.collect_relation_values(plan, depth)?;
                self.observe_list_relation(operation, values, arguments, depth)
            }
            "sum" | "min" | "max" => {
                let values = self.collect_relation_values(plan, depth)?;
                self.observe_list_relation(operation, values, arguments, depth)
            }
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }

    fn observe_sorted_relation(
        &mut self,
        plan: &RelationPlan,
        operation: &str,
        arguments: &[Value],
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        match operation {
            "last" => {
                let mut state = RelationLastState::new();
                self.for_each_sorted_relation(plan, depth, |_, value| {
                    state.push(value);
                    Ok(true)
                })?;
                Ok(Value::Option(state.finish().map(Box::new)))
            }
            "first" => {
                let mut first = None;
                self.for_each_sorted_relation(plan, depth, |_, value| {
                    first = Some(value);
                    Ok(false)
                })?;
                Ok(Value::Option(first.map(Box::new)))
            }
            "one" => {
                let predicate = arguments.first();
                let mut found = None;
                self.for_each_sorted_relation(plan, depth, |context, value| {
                    if let Some(predicate) = predicate {
                        context.step()?;
                        match context.invoke_predicate(predicate, value.clone(), depth + 1)? {
                            Value::Bool(true) => {}
                            Value::Bool(false) => return Ok(true),
                            _ => return Err(error("ORNA-EVAL-TYPE")),
                        }
                    }
                    if found.is_some() {
                        return Err(error("ORNA-EVAL-RELATION-ONE-MULTIPLE"));
                    }
                    found = Some(value);
                    Ok(true)
                })?;
                found.ok_or_else(|| error("ORNA-EVAL-RELATION-ONE-ZERO"))
            }
            "count" => {
                let mut count = 0usize;
                self.for_each_sorted_relation(plan, depth, |context, _| {
                    count = count
                        .checked_add(1)
                        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
                    context.items(count)?;
                    Ok(true)
                })?;
                Ok(Value::Int(BigInt::from(count)))
            }
            "every" | "exists" => {
                let predicate = arguments
                    .first()
                    .ok_or_else(|| error("ORNA-EVAL-ARGUMENT"))?;
                let want_exists = operation == "exists";
                let mut result = !want_exists;
                self.for_each_sorted_relation(plan, depth, |context, value| {
                    context.step()?;
                    let value = context.invoke_predicate(predicate, value, depth + 1)?;
                    let Value::Bool(value) = value else {
                        return Err(error("ORNA-EVAL-TYPE"));
                    };
                    if value == want_exists {
                        result = value;
                        return Ok(false);
                    }
                    Ok(true)
                })?;
                Ok(Value::Bool(result))
            }
            "window" | "sum" | "min" | "max" => {
                let mut values = Vec::new();
                self.for_each_sorted_relation(plan, depth, |context, value| {
                    values.push(value);
                    context.items(values.len())?;
                    Ok(true)
                })?;
                self.observe_list_relation(operation, values, arguments, depth)
            }
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }

    fn observe_list_relation(
        &mut self,
        operation: &str,
        values: Vec<Value>,
        arguments: &[Value],
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        let mut inputs = vec![Value::List(values)];
        inputs.extend(arguments.iter().cloned());
        self.collection(operation, inputs, depth)
    }

    fn collect_relation_values(
        &mut self,
        plan: &RelationPlan,
        depth: usize,
    ) -> Result<Vec<Value>, EvaluationError> {
        let mut values = Vec::new();
        self.for_each_relation_value(plan, depth, |context, value| {
            values.push(value);
            context.items(values.len())?;
            Ok(true)
        })?;
        Ok(values)
    }

    fn for_each_relation_value(
        &mut self,
        plan: &RelationPlan,
        depth: usize,
        mut visit: impl FnMut(&mut Self, Value) -> Result<bool, EvaluationError>,
    ) -> Result<(), EvaluationError> {
        if plan
            .stages
            .iter()
            .any(|stage| matches!(stage, RelationStage::Take(0)))
        {
            self.validate_relation_plan_sources(plan)?;
            return Ok(());
        }
        let mut counters = vec![0usize; plan.stages.len()];
        let mut distinct_seen = vec![HashSet::new(); plan.stages.len()];
        let mut pair_previous = vec![None; plan.stages.len()];
        let mut window_states = (0..plan.stages.len())
            .map(|_| None)
            .collect::<Vec<Option<RelationWindowState>>>();
        let mut seen = 0usize;
        self.for_each_relation_source_value(plan, depth, &mut |context, value| {
            context.step()?;
            seen = seen
                .checked_add(1)
                .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
            context.items(seen)?;
            let rows = context.apply_relation_stages(
                value,
                &plan.stages,
                &mut counters,
                &mut distinct_seen,
                &mut pair_previous,
                &mut window_states,
                0,
                depth + 1,
            )?;
            for row in rows {
                match row {
                    RelationRow::Skip => {}
                    RelationRow::End => return Ok(false),
                    RelationRow::Yield(value) => {
                        if !visit(context, value)? {
                            return Ok(false);
                        }
                    }
                }
            }
            Ok(true)
        })
    }

    fn for_each_relation_source_value(
        &mut self,
        plan: &RelationPlan,
        depth: usize,
        visit: &mut dyn FnMut(&mut Self, Value) -> Result<bool, EvaluationError>,
    ) -> Result<(), EvaluationError> {
        if let Some((left, right)) = &plan.source_union {
            let mut keep_going = true;
            self.for_each_relation_value(left, depth, |context, value| {
                keep_going = visit(context, value)?;
                Ok(keep_going)
            })?;
            if !keep_going {
                return Ok(());
            }
            self.for_each_relation_value(right, depth, |context, value| {
                keep_going = visit(context, value)?;
                Ok(keep_going)
            })?;
            return Ok(());
        }

        if let Some(values) = &plan.source_values {
            for value in values.iter().cloned() {
                self.step()?;
                if !visit(self, value)? {
                    return Ok(());
                }
            }
            return Ok(());
        }

        let mut after = None;
        loop {
            // Relation work has its own cancellation checkpoints. A plan
            // must remain interruptible even when its callbacks are absent,
            // short-circuiting, or otherwise do not execute.
            self.step()?;
            let page = self.relation_page(&plan.source, plan.source_identity, after.as_deref())?;
            let page_len = page.rows.len();
            for canonical in page.rows {
                self.step()?;
                let value = Value::from_canonical(&canonical, self, depth + 1)?;
                if !visit(self, value)? {
                    return Ok(());
                }
            }
            let Some(next) = page.next else {
                return Ok(());
            };
            if page_len == 0 {
                return Err(error("ORNA-EVAL-VALUE"));
            }
            after = Some(next);
        }
    }

    fn relation_filter_passes<'a>(
        &mut self,
        value: &Value,
        predicates: impl Iterator<Item = &'a Value>,
        depth: usize,
    ) -> Result<bool, EvaluationError> {
        for predicate in predicates {
            self.step()?;
            let result = self.invoke_predicate(predicate, value.clone(), depth)?;
            let Value::Bool(result) = result else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            if !result {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn apply_relation_stages(
        &mut self,
        mut value: Value,
        stages: &[RelationStage],
        counters: &mut [usize],
        distinct_seen: &mut [HashSet<Vec<u8>>],
        pair_previous: &mut [Option<Value>],
        window_states: &mut [Option<RelationWindowState>],
        stage_offset: usize,
        depth: usize,
    ) -> Result<Vec<RelationRow>, EvaluationError> {
        for (local_index, stage) in stages.iter().enumerate() {
            let index = stage_offset + local_index;
            match stage {
                RelationStage::Filter(predicates) => {
                    if !self.relation_filter_passes(&value, predicates.iter(), depth + 1)? {
                        return Ok(rejected_relation_rows(stages, counters, stage_offset));
                    }
                }
                RelationStage::SharedFilter(batch) => {
                    let predicates = batch.chunks().iter().flat_map(|chunk| chunk.iter());
                    if !self.relation_filter_passes(&value, predicates, depth + 1)? {
                        return Ok(rejected_relation_rows(stages, counters, stage_offset));
                    }
                }
                RelationStage::Map(transform) => {
                    value = self.invoke_predicate(transform, value, depth + 1)?;
                }
                RelationStage::FlatMap(transform) => {
                    self.step()?;
                    let mapped = self.invoke_predicate(transform, value, depth + 1)?;
                    let suffix = &stages[local_index + 1..];
                    return self.apply_relation_flat_map(
                        mapped,
                        stages,
                        suffix,
                        counters,
                        distinct_seen,
                        pair_previous,
                        window_states,
                        stage_offset,
                        index + 1,
                        depth + 1,
                    );
                }
                RelationStage::BucketBy(_) => return Err(error("ORNA-EVAL-UNSUPPORTED")),
                RelationStage::Distinct => {
                    let seen = &mut distinct_seen[index];
                    if !seen.insert(distinct_identity(&value)?) {
                        return Ok(rejected_relation_rows(stages, counters, stage_offset));
                    }
                    self.items(seen.len())?;
                }
                RelationStage::Pairs => {
                    let Some(previous) = pair_previous[index].replace(value.clone()) else {
                        return Ok(rejected_relation_rows(stages, counters, stage_offset));
                    };
                    self.items(2)?;
                    value = Value::Tuple(vec![previous, value]);
                }
                RelationStage::Window(size, step) => {
                    let state = window_states[index].get_or_insert_with(|| {
                        RelationWindowState::try_new(*size, *step)
                            .expect("window stage parameters are validated")
                    });
                    self.items(state.next_item_bound())?;
                    let Some(window) = state.push(value) else {
                        return Ok(rejected_relation_rows(stages, counters, stage_offset));
                    };
                    value = Value::List(window);
                }
                RelationStage::Drop(count) => {
                    if counters[index] < *count {
                        counters[index] += 1;
                        return Ok(rejected_relation_rows(stages, counters, stage_offset));
                    }
                }
                RelationStage::Take(count) => {
                    if counters[index] >= *count {
                        return Ok(vec![RelationRow::End]);
                    }
                    counters[index] += 1;
                }
                RelationStage::SortBy(_) => {
                    return Err(error("ORNA-EVAL-UNSUPPORTED"));
                }
            }
        }
        let mut rows = vec![RelationRow::Yield(value)];
        if stages.iter().enumerate().any(|(offset, stage)| {
            matches!(stage, RelationStage::Take(count) if counters[stage_offset + offset] >= *count)
        }) {
            rows.push(RelationRow::End);
        }
        Ok(rows)
    }

    fn apply_relation_flat_map(
        &mut self,
        mapped: Value,
        stages: &[RelationStage],
        suffix: &[RelationStage],
        counters: &mut [usize],
        distinct_seen: &mut [HashSet<Vec<u8>>],
        pair_previous: &mut [Option<Value>],
        window_states: &mut [Option<RelationWindowState>],
        stage_offset: usize,
        suffix_offset: usize,
        depth: usize,
    ) -> Result<Vec<RelationRow>, EvaluationError> {
        let mut rows = Vec::new();
        match mapped {
            Value::List(inner) => {
                self.items(inner.len())?;
                for inner_value in inner {
                    if inner_value.contains_callable() {
                        return Err(error("ORNA-EVAL-UNSUPPORTED"));
                    }
                    self.step()?;
                    let inner_rows = self.apply_relation_stages(
                        inner_value,
                        suffix,
                        counters,
                        distinct_seen,
                        pair_previous,
                        window_states,
                        suffix_offset,
                        depth + 1,
                    )?;
                    let ended = inner_rows.iter().any(|row| matches!(row, RelationRow::End));
                    rows.extend(inner_rows);
                    self.items(rows.len())?;
                    if ended {
                        break;
                    }
                }
            }
            Value::Relation(plan) => {
                // The reference fixes flat_map order but leaves query-valued
                // callbacks implicit. Treat a returned relation as a lateral
                // child: finish its scoped scan before advancing the outer row.
                self.for_each_relation_value(&plan, depth + 1, |context, inner_value| {
                    if inner_value.contains_callable() {
                        return Err(error("ORNA-EVAL-UNSUPPORTED"));
                    }
                    let inner_rows = context.apply_relation_stages(
                        inner_value,
                        suffix,
                        counters,
                        distinct_seen,
                        pair_previous,
                        window_states,
                        suffix_offset,
                        depth + 1,
                    )?;
                    let ended = inner_rows.iter().any(|row| matches!(row, RelationRow::End));
                    rows.extend(inner_rows);
                    context.items(rows.len())?;
                    Ok(!ended)
                })?;
            }
            _ => return Err(error("ORNA-EVAL-TYPE")),
        }
        if !rows.iter().any(|row| matches!(row, RelationRow::End))
            && stages.iter().enumerate().any(|(offset, stage)| {
                matches!(stage, RelationStage::Take(count) if counters[stage_offset + offset] >= *count)
            })
        {
            rows.push(RelationRow::End);
        }
        Ok(rows)
    }

    fn for_each_sorted_relation(
        &mut self,
        plan: &RelationPlan,
        depth: usize,
        visit: impl FnMut(&mut Self, Value) -> Result<bool, EvaluationError>,
    ) -> Result<(), EvaluationError> {
        if plan
            .stages
            .iter()
            .any(|stage| matches!(stage, RelationStage::Take(0)))
        {
            self.validate_relation_plan_sources(plan)?;
            return Ok(());
        }
        let Some(sort_index) = plan
            .stages
            .iter()
            .position(|stage| matches!(stage, RelationStage::SortBy(_)))
        else {
            return self.for_each_relation_value(plan, depth, visit);
        };
        let prefix = RelationPlan {
            source: plan.source.clone(),
            source_identity: plan.source_identity,
            source_union: plan.source_union.clone(),
            source_values: plan.source_values.clone(),
            stages: plan.stages[..sort_index].to_vec(),
        };
        let RelationStage::SortBy(key) = &plan.stages[sort_index] else {
            unreachable!("sort stage index")
        };
        let mut values = Vec::new();
        self.for_each_relation_value(&prefix, depth, |context, value| {
            values.push(value);
            context.items(values.len())?;
            Ok(true)
        })?;
        let sorted = self.collection("sort_by", vec![Value::List(values), key.clone()], depth)?;
        let Value::List(sorted) = sorted else {
            unreachable!("sort_by returns a list")
        };
        let suffix = &plan.stages[sort_index + 1..];
        let mut distinct_seen = vec![HashSet::new(); plan.stages.len()];
        let mut pair_previous = vec![None; plan.stages.len()];
        let mut window_states = (0..plan.stages.len())
            .map(|_| None)
            .collect::<Vec<Option<RelationWindowState>>>();
        self.for_each_buffered_relation(
            sorted,
            suffix,
            sort_index + 1,
            depth,
            &mut distinct_seen,
            &mut pair_previous,
            &mut window_states,
            visit,
        )
    }

    fn for_each_buffered_relation(
        &mut self,
        values: Vec<Value>,
        stages: &[RelationStage],
        stage_offset: usize,
        depth: usize,
        distinct_seen: &mut [HashSet<Vec<u8>>],
        pair_previous: &mut [Option<Value>],
        window_states: &mut [Option<RelationWindowState>],
        visit: impl FnMut(&mut Self, Value) -> Result<bool, EvaluationError>,
    ) -> Result<(), EvaluationError> {
        let Some(sort_index) = stages
            .iter()
            .position(|stage| matches!(stage, RelationStage::SortBy(_)))
        else {
            return self.for_each_buffered_stages(
                values,
                stages,
                stage_offset,
                depth,
                distinct_seen,
                pair_previous,
                window_states,
                visit,
            );
        };
        let prefix = &stages[..sort_index];
        let mut upstream = Vec::new();
        self.for_each_buffered_stages(
            values,
            prefix,
            stage_offset,
            depth,
            distinct_seen,
            pair_previous,
            window_states,
            |context, value| {
                upstream.push(value);
                context.items(upstream.len())?;
                Ok(true)
            },
        )?;
        let RelationStage::SortBy(key) = &stages[sort_index] else {
            unreachable!("sort stage index")
        };
        let sorted = self.collection("sort_by", vec![Value::List(upstream), key.clone()], depth)?;
        let Value::List(sorted) = sorted else {
            unreachable!("sort_by returns a list")
        };
        self.for_each_buffered_relation(
            sorted,
            &stages[sort_index + 1..],
            stage_offset + sort_index + 1,
            depth,
            distinct_seen,
            pair_previous,
            window_states,
            visit,
        )
    }

    fn for_each_buffered_stages(
        &mut self,
        values: Vec<Value>,
        stages: &[RelationStage],
        stage_offset: usize,
        depth: usize,
        distinct_seen: &mut [HashSet<Vec<u8>>],
        pair_previous: &mut [Option<Value>],
        window_states: &mut [Option<RelationWindowState>],
        mut visit: impl FnMut(&mut Self, Value) -> Result<bool, EvaluationError>,
    ) -> Result<(), EvaluationError> {
        if stages
            .iter()
            .any(|stage| matches!(stage, RelationStage::Take(0)))
        {
            return Ok(());
        }
        let mut counters = vec![0usize; stage_offset + stages.len()];
        for value in values {
            // Buffered relation stages can run without a predicate or visitor
            // that performs its own evaluator step. Keep this materialized
            // path as cancellation-aware as the paged source path.
            self.step()?;
            let rows = self.apply_relation_stages(
                value,
                stages,
                &mut counters,
                distinct_seen,
                pair_previous,
                window_states,
                stage_offset,
                depth + 1,
            )?;
            for row in rows {
                match row {
                    RelationRow::Skip => {}
                    RelationRow::End => return Ok(()),
                    RelationRow::Yield(value) => {
                        if !visit(self, value)? {
                            return Ok(());
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn relation_page(
        &mut self,
        source: &str,
        scope: RelationReadScope,
        after: Option<&[u8]>,
    ) -> Result<RelationPage, EvaluationError> {
        let remaining = self.limits.max_steps.saturating_sub(self.steps);
        let mut budget = StepBudget::new(remaining);
        let result = self
            .effects
            .as_deref_mut()
            .ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))
            .and_then(|effects| {
                effects.scan_relation_page_scoped(source, scope, after, 1, &mut budget)
            });
        let debited = remaining.saturating_sub(budget.remaining());
        self.steps = self
            .steps
            .checked_add(debited)
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        let page = result?.ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?;
        if let (Some(after), Some(next)) = (after, page.next.as_deref())
            && next <= after
        {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        Ok(page)
    }

    fn validate_relation_plan_sources(
        &mut self,
        plan: &RelationPlan,
    ) -> Result<(), EvaluationError> {
        if let Some((left, right)) = &plan.source_union {
            self.validate_relation_plan_sources(left)?;
            self.validate_relation_plan_sources(right)
        } else if plan.source_values.is_some() {
            Ok(())
        } else {
            self.effects
                .as_deref_mut()
                .ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?
                .validate_relation_source(&plan.source)
        }
    }

    fn ui_action(
        &mut self,
        arguments: &[orna_syntax_v1::Argument],
        scope: &mut Scope,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(arguments.len())?;
        let mut action_id = None;
        let mut input_type = None;
        let mut debug_kind = None;
        let mut positional = 0usize;
        for argument in arguments {
            match argument.name.as_deref() {
                Some("action_id") if action_id.is_none() => {
                    let value = self.evaluate(&argument.value, scope, depth + 1)?;
                    let Value::String(value) = value else {
                        return Err(error("ORNA-EVAL-TYPE"));
                    };
                    action_id = Some(value);
                }
                Some("as") if input_type.is_none() => {
                    let Some(name) = function_name(&argument.value) else {
                        return Err(error("ORNA-EVAL-ARGUMENT"));
                    };
                    input_type = ui_input_type_name(&name).map(str::to_owned);
                }
                Some("debug_kind") if debug_kind.is_none() => {
                    let value = self.evaluate(&argument.value, scope, depth + 1)?;
                    match value {
                        Value::String(value) => debug_kind = Some(value),
                        Value::Null => debug_kind = Some(String::new()),
                        _ => return Err(error("ORNA-EVAL-TYPE")),
                    }
                }
                None if positional == 0 && action_id.is_none() => {
                    let value = self.evaluate(&argument.value, scope, depth + 1)?;
                    let Value::String(value) = value else {
                        return Err(error("ORNA-EVAL-TYPE"));
                    };
                    action_id = Some(value);
                    positional += 1;
                }
                _ => return Err(error("ORNA-EVAL-ARGUMENT")),
            }
        }
        let action_id = action_id
            .filter(|value| !value.is_empty())
            .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
        let input_type = input_type.ok_or_else(|| error("ORNA-EVAL-ARGUMENT"))?;
        self.string(action_id.clone())?;
        self.string(input_type.clone())?;
        let mut fields = BTreeMap::new();
        fields.insert("action_id".into(), Value::String(action_id));
        fields.insert("input_type".into(), Value::String(input_type));
        fields.insert(
            "debug_kind".into(),
            match debug_kind {
                Some(value) if !value.is_empty() => Value::String(value),
                _ => Value::Null,
            },
        );
        Ok(Value::Record(fields))
    }

    fn ui_node(&mut self, values: Vec<Value>) -> Result<Value, EvaluationError> {
        let [
            Value::String(kind),
            Value::Record(properties),
            Value::List(children),
            Value::List(actions),
            Value::List(property_types),
        ] = values.as_slice()
        else {
            return Err(error("ORNA-EVAL-TYPE"));
        };
        let contract_name = match kind.as_str() {
            "field" | "text" | "rows" | "cols" | "stack" | "details" | "table" | "tree"
            | "code" | "diff" | "chart" | "button" | "form" | "input" => kind,
            _ => return Err(error("ORNA-EVAL-ARGUMENT")),
        };
        self.items(properties.len() + children.len() + actions.len() + property_types.len() + 5)?;
        self.string(contract_name.clone())?;

        let mut type_overrides = BTreeMap::new();
        for hint in property_types {
            let Value::Tuple(pair) = hint else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            let [Value::String(name), Value::String(type_name)] = pair.as_slice() else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            if name.is_empty()
                || type_name.is_empty()
                || !properties.contains_key(name)
                || type_overrides
                    .insert(name.clone(), type_name.clone())
                    .is_some()
            {
                return Err(error("ORNA-EVAL-ARGUMENT"));
            }
            self.string(name.clone())?;
            self.string(type_name.clone())?;
        }

        let mut action_map = BTreeMap::new();
        for action in actions {
            let Value::Tuple(pair) = action else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            let [Value::String(name), descriptor] = pair.as_slice() else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            if name.is_empty() || !is_ui_action_descriptor(descriptor) {
                return Err(error("ORNA-EVAL-ARGUMENT"));
            }
            self.string(name.clone())?;
            if action_map
                .insert(name.clone(), descriptor.clone())
                .is_some()
            {
                return Err(error("ORNA-EVAL-ARGUMENT"));
            }
        }

        if kind == "input" {
            let Value::Record(descriptor) = action_map
                .get("change")
                .ok_or_else(|| error("ORNA-EVAL-ARGUMENT"))?
            else {
                return Err(error("ORNA-EVAL-ARGUMENT"));
            };
            let Some(Value::String(input_type)) = descriptor.get("input_type") else {
                return Err(error("ORNA-EVAL-ARGUMENT"));
            };
            match properties.get("value") {
                Some(Value::Option(Some(value)))
                    if ui_value_type_name(value) != input_type.as_str() =>
                {
                    return Err(error("ORNA-EVAL-ARGUMENT"));
                }
                Some(Value::Option(None) | Value::Null) => {
                    type_overrides
                        .entry("value".into())
                        .or_insert_with(|| format!("std.option<{input_type}>"));
                }
                Some(Value::Option(Some(_))) => {}
                _ => return Err(error("ORNA-EVAL-TYPE")),
            }
        }

        let properties = properties
            .iter()
            .map(|(name, value)| {
                self.string(name.clone())?;
                value.clone().canonical()?;
                let type_name = type_overrides
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| ui_value_type_name(value));
                if !ui_property_type_matches(&type_name, value) {
                    return Err(error("ORNA-EVAL-TYPE"));
                }
                let mut typed = BTreeMap::new();
                typed.insert("type".into(), Value::String(type_name));
                typed.insert("value".into(), value.clone());
                Ok((name.clone(), Value::Record(typed)))
            })
            .collect::<Result<BTreeMap<_, _>, EvaluationError>>()?;

        let mut slots = BTreeMap::new();
        for child in children {
            if !is_ui_presentation_node(child, 1, self.limits.max_depth)? {
                return Err(error("ORNA-EVAL-TYPE"));
            }
        }
        slots.insert("content".into(), Value::List(children.clone()));

        let mut contract = BTreeMap::new();
        contract.insert(
            "id".into(),
            Value::String(format!("std.ui.{contract_name}@1")),
        );
        contract.insert(
            "name".into(),
            Value::String(format!("std.ui.{contract_name}")),
        );
        contract.insert("version".into(), Value::String("1.0".into()));

        let mut node = BTreeMap::new();
        node.insert("kind".into(), Value::String("node".into()));
        node.insert("contract".into(), Value::Record(contract));
        node.insert("properties".into(), Value::Record(properties));
        node.insert("slots".into(), Value::Record(slots));
        node.insert("actions".into(), Value::Record(action_map));
        Ok(Value::Record(node))
    }

    fn decode_with_type_witness(
        &mut self,
        codec: &str,
        supports_options: bool,
        arguments: &[orna_syntax_v1::Argument],
        input: Option<Value>,
        scope: &mut Scope,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(arguments.len() + usize::from(input.is_some()))?;
        let mut supplied_input = input;
        let mut witness: Option<CodecTypeWitness> = None;
        let mut ignore_unknown_fields = false;
        let mut saw_options = false;
        let mut positional = usize::from(supplied_input.is_some());
        for argument in arguments {
            match argument.name.as_deref() {
                Some("as") if witness.is_none() => {
                    witness = codec_type_witness(&argument.value);
                    if witness.is_none() {
                        return Err(error("ORNA-EVAL-ARGUMENT"));
                    }
                }
                Some("ignore_unknown_fields")
                    if codec == "json" && supports_options && !saw_options =>
                {
                    let Value::Bool(value) = self.evaluate(&argument.value, scope, depth + 1)?
                    else {
                        return Err(error("ORNA-EVAL-TYPE"));
                    };
                    ignore_unknown_fields = value;
                    saw_options = true;
                }
                Some("input") if supplied_input.is_none() => {
                    supplied_input = Some(self.evaluate(&argument.value, scope, depth + 1)?);
                }
                None if supplied_input.is_none() && positional == 0 => {
                    supplied_input = Some(self.evaluate(&argument.value, scope, depth + 1)?);
                    positional += 1;
                }
                _ => return Err(error("ORNA-EVAL-ARGUMENT")),
            }
            if self.transfer.is_some() {
                return Ok(Value::Null);
            }
        }
        let witness = witness.ok_or_else(|| error("ORNA-EVAL-ARGUMENT"))?;
        let input = supplied_input.ok_or_else(|| error("ORNA-EVAL-ARGUMENT"))?;
        let decoded = match (codec, input) {
            ("json", Value::String(input)) => self.decode_json_with_witness(
                &input,
                &witness,
                ignore_unknown_fields,
                scope,
                depth + 1,
            )?,
            ("orna", Value::String(input)) => {
                self.orna_codec("__decode", vec![Value::String(input)])?
            }
            ("ovb", Value::Blob(input)) => self.ovb_codec("__decode", vec![Value::Blob(input)])?,
            ("json" | "orna", _) => return Err(error("ORNA-EVAL-TYPE")),
            ("ovb", _) => return Err(error("ORNA-EVAL-TYPE")),
            _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
        };
        if codec_type_matches(&decoded, &witness, &scope.3) {
            Ok(decoded)
        } else {
            Err(error("ORNA-EVAL-VALUE"))
        }
    }

    fn decode_json_with_witness(
        &mut self,
        input: &str,
        witness: &CodecTypeWitness,
        ignore_unknown_fields: bool,
        scope: &mut Scope,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        let input = self.string(input.to_owned())?;
        let node = parse_json_node(&input)?;
        let CodecTypeWitness::Named(witness) = witness else {
            return json_node_to_value(node, self, depth);
        };
        let Some(definition) = scope.3.get(witness).cloned() else {
            return json_node_to_value(node, self, depth);
        };
        if !definition.variants.is_empty() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        self.depth(depth)?;
        self.step()?;
        let JsonNode::Object(entries) = node else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        self.items(entries.len())?;
        self.items(definition.fields.len())?;

        let mut supplied = BTreeMap::new();
        for (name, value) in entries {
            self.string(name.clone())?;
            if definition
                .fields
                .iter()
                .any(|field| field.public && field.name == name)
            {
                supplied.insert(name, value);
            } else if ignore_unknown_fields {
                // Ignored fields still consume evaluator limits so a large
                // unknown subtree cannot bypass the bounded decoder.
                let _ = json_node_to_value(value, self, depth + 1)?;
            } else {
                return Err(error("ORNA-EVAL-VALUE"));
            }
        }

        let mut construction_scope = scope.clone();
        let mut fields = Vec::with_capacity(definition.fields.len());
        for field in &definition.fields {
            let value = if field.public {
                if let Some(node) = supplied.remove(&field.name) {
                    json_node_to_value(node, self, depth + 1)?
                } else if let Some(default) = &field.default {
                    let previous_namespace = self.namespace.clone();
                    self.namespace = definition.owner.clone();
                    let result = self.evaluate(default, &mut construction_scope, depth + 1);
                    self.namespace = previous_namespace;
                    result?
                } else {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
            } else if let Some(default) = &field.default {
                let previous_namespace = self.namespace.clone();
                self.namespace = definition.owner.clone();
                let result = self.evaluate(default, &mut construction_scope, depth + 1);
                self.namespace = previous_namespace;
                result?
            } else {
                return Err(error("ORNA-EVAL-VALUE"));
            };
            if self.transfer.is_some() {
                return Ok(Value::Null);
            }
            construction_scope
                .0
                .insert(field.name.clone(), value.clone());
            construction_scope.1.remove(&field.name);
            fields.push((field.field_id.clone(), value));
        }
        if !supplied.is_empty() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        Ok(Value::NominalRecord {
            type_id: definition.type_id,
            fields,
        })
    }

    fn call(
        &mut self,
        callee: &Expr,
        arguments: &[orna_syntax_v1::Argument],
        input: Option<Value>,
        scope: &mut Scope,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        let resolved_name = self.resolve_function_name(callee, scope);
        if let Some((codec, supports_options)) =
            resolved_name.as_deref().and_then(codec_decode_operation)
        {
            return self.decode_with_type_witness(
                codec,
                supports_options,
                arguments,
                input,
                scope,
                depth,
            );
        }
        if function_name(callee).as_deref() == Some("std.ui.action") {
            if input.is_some() {
                return Err(error("ORNA-EVAL-ARGUMENT"));
            }
            return self.ui_action(arguments, scope, depth);
        }
        // `Some(value)` constructs the language's core Optional value. It is
        // available without importing std and remains shadowable by an
        // explicit lexical or admitted function binding.
        if matches!(callee, Expr::Name { text, .. } if text == "Some")
            && !scope.0.contains_key("Some")
            && self.resolve_function_name(callee, scope).is_none()
        {
            if input.is_some() || arguments.len() != 1 || arguments[0].name.is_some() {
                return Err(error("ORNA-EVAL-ARGUMENT"));
            }
            let value = self.evaluate(&arguments[0].value, scope, depth + 1)?;
            if self.transfer.is_some() {
                return Ok(Value::Null);
            }
            return Ok(Value::Option(Some(Box::new(value))));
        }
        // `now()` is an activation-scoped intrinsic. It is deliberately
        // offered only through the existing effect boundary so the evaluator
        // never reads a wall clock and callers without an activation handler
        // fail closed as unsupported. A lexical/function binding named
        // `now` retains precedence and therefore shadows this intrinsic.
        if matches!(callee, Expr::Name { text, .. } if text == "now")
            && !scope.0.contains_key("now")
            && self.resolve_function_name(callee, scope).is_none()
            && self.effects.is_some()
        {
            if input.is_some() || !arguments.is_empty() {
                return Err(error("ORNA-EVAL-ARGUMENT"));
            }
            let remaining = self.limits.max_steps.saturating_sub(self.steps);
            let mut budget = StepBudget::new(remaining);
            let result = self
                .effects
                .as_deref_mut()
                .expect("checked effect handler")
                .handle_with_budget(callee, &[], &mut budget);
            let debited = remaining - budget.remaining();
            self.steps = self
                .steps
                .checked_add(debited)
                .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
            if let Some(value) = result? {
                return self.effect_value(&value);
            }
        }
        // `uuid7()` is an activation-scoped intrinsic. Like `now()`, it is
        // available only through the effect boundary: the evaluator never
        // supplies a clock or random fallback. Lexical and admitted function
        // bindings retain precedence over the root intrinsic.
        if matches!(callee, Expr::Name { text, .. } if text == "uuid7")
            && !scope.0.contains_key("uuid7")
            && self.resolve_function_name(callee, scope).is_none()
            && self.effects.is_some()
        {
            if input.is_some() || !arguments.is_empty() {
                return Err(error("ORNA-EVAL-ARGUMENT"));
            }
            let remaining = self.limits.max_steps.saturating_sub(self.steps);
            let mut budget = StepBudget::new(remaining);
            let result = self
                .effects
                .as_deref_mut()
                .expect("checked effect handler")
                .handle_with_budget(callee, &[], &mut budget);
            let debited = remaining - budget.remaining();
            self.steps = self
                .steps
                .checked_add(debited)
                .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
            if let Some(value) = result? {
                return self.effect_value(&value);
            }
        }
        if matches!(callee, Expr::Name { text, .. } if text == "error")
            && !scope.0.contains_key("error")
            && self.resolve_function_name(callee, scope).is_none()
        {
            if input.is_some() || arguments.iter().any(|argument| argument.name.is_none()) {
                return Err(error("ORNA-EVAL-ARGUMENT"));
            }
            let mut supplied = BTreeMap::new();
            for argument in arguments {
                let name = argument
                    .name
                    .as_ref()
                    .expect("unnamed arguments rejected above");
                if !matches!(name.as_str(), "code" | "message" | "cause")
                    || supplied.contains_key(name)
                {
                    return Err(error("ORNA-EVAL-ARGUMENT"));
                }
                let value = self.evaluate(&argument.value, scope, depth + 1)?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                supplied.insert(name.clone(), value);
            }
            let Value::String(code) = supplied
                .remove("code")
                .ok_or_else(|| error("ORNA-EVAL-ARGUMENT"))?
            else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            let Value::String(message) = supplied
                .remove("message")
                .ok_or_else(|| error("ORNA-EVAL-ARGUMENT"))?
            else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            let causes = match supplied.remove("cause") {
                None => Vec::new(),
                Some(Value::Error(cause)) => vec![cause.handler_error()?],
                Some(_) => return Err(error("ORNA-EVAL-TYPE")),
            };
            let value = CanonicalErrorValue::new(code, message, causes, BTreeMap::new())
                .map_err(|_| error("ORNA-EVAL-VALUE"))?;
            return Ok(Value::Error(EvaluationError::from_canonical(value)));
        }

        // `fail(error_value)` is an abrupt intrinsic, not an ordinary
        // callable. Error values only exist while a recovery handler is
        // running, so re-emitting one preserves the original diagnostic and
        // lets the surrounding `|?` boundary decide whether to handle it.
        if matches!(callee, Expr::Name { text, .. } if text == "fail")
            && !scope.0.contains_key("fail")
            && self.resolve_function_name(callee, scope).is_none()
        {
            if input.is_some() || arguments.len() != 1 {
                return Err(error("ORNA-EVAL-ARGUMENT"));
            }
            let value = self.evaluate(&arguments[0].value, scope, depth + 1)?;
            if self.transfer.is_some() {
                return Ok(Value::Null);
            }
            return match value {
                Value::Error(failure) => Err(failure),
                _ => Err(error("ORNA-EVAL-TYPE")),
            };
        }
        if is_relation_source(callee) {
            if input.is_some() || arguments.len() != 1 {
                return Err(error("ORNA-EVAL-ARGUMENT"));
            }
            let source = self.evaluate(&arguments[0].value, scope, depth + 1)?;
            let Value::String(source) = source else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            return Ok(Value::Relation(RelationPlan::new(source)));
        }
        if let Some(result) = self.relation_call(callee, arguments, input.clone(), scope, depth) {
            return result;
        }
        let root_collection =
            root_collection_name(callee).filter(|name| !scope.0.contains_key(*name));
        let resolved_function = self.resolve_function_name(callee, scope);
        if let Some(operation_name) = resolved_function.as_deref()
            && let Some(operation) =
                orna_sys_v1::system_host_operation_registry().operation(operation_name)
        {
            if self.effects.is_none() {
                return Err(error("ORNA-EVAL-UNSUPPORTED"));
            }
            if input.is_some() || arguments.len() != operation.parameters.len() {
                return Err(error("ORNA-EVAL-ARGUMENT"));
            }
            let positional = arguments.iter().all(|argument| argument.name.is_none());
            let ordered_arguments = if positional {
                arguments.iter().collect::<Vec<_>>()
            } else {
                let mut ordered = Vec::with_capacity(arguments.len());
                for parameter in &operation.parameters {
                    let mut matches = arguments
                        .iter()
                        .filter(|argument| argument.name.as_deref() == Some(parameter.as_str()));
                    let Some(argument) = matches.next() else {
                        return Err(error("ORNA-EVAL-ARGUMENT"));
                    };
                    if matches.next().is_some() {
                        return Err(error("ORNA-EVAL-ARGUMENT"));
                    }
                    ordered.push(argument);
                }
                if ordered.len() != arguments.len() {
                    return Err(error("ORNA-EVAL-ARGUMENT"));
                }
                ordered
            };
            let values = ordered_arguments
                .iter()
                .map(|argument| {
                    self.evaluate(&argument.value, scope, depth + 1)?
                        .canonical()
                })
                .collect::<Result<Vec<_>, EvaluationError>>()?;
            let remaining = self.limits.max_steps.saturating_sub(self.steps);
            let mut budget = StepBudget::new(remaining);
            let result = self
                .effects
                .as_deref_mut()
                .expect("host effects were checked above")
                .handle_registered_with_cancellation_and_budget(
                    operation_name,
                    callee,
                    &values,
                    &mut budget,
                    self.cancellation,
                );
            let debited = remaining - budget.remaining();
            self.steps = self
                .steps
                .checked_add(debited)
                .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
            if let Some(value) = result? {
                return self.effect_value(&value);
            }
        }
        let source_export_available =
            function_name(callee).is_some_and(|name| self.functions.contains_key(&name));
        let native_binding = registered_standard_binding(
            callee,
            resolved_function.as_deref(),
            scope,
            !self.restrict_function_names,
            source_export_available,
            captured_timezone_snapshot_matches(self.functions),
        );
        let native_math = native_binding
            .filter(|binding| binding.kind == StandardBindingKind::Math)
            .map(|binding| binding.operation);
        let native_text = native_binding
            .filter(|binding| binding.kind == StandardBindingKind::Text)
            .map(|binding| binding.operation);
        let native_bits = native_binding
            .filter(|binding| binding.kind == StandardBindingKind::Bits)
            .map(|binding| binding.operation);
        let native_stream = native_binding
            .filter(|binding| binding.kind == StandardBindingKind::Stream)
            .map(|binding| binding.operation);
        let native_concurrent = native_binding
            .filter(|binding| binding.kind == StandardBindingKind::Concurrent)
            .map(|binding| binding.operation);
        let native_ui = native_binding
            .filter(|binding| binding.kind == StandardBindingKind::Ui)
            .map(|binding| binding.operation);
        let native_stats = native_binding
            .filter(|binding| binding.kind == StandardBindingKind::Stats)
            .map(|binding| binding.operation);
        let native_time = native_binding
            .filter(|binding| binding.kind == StandardBindingKind::Time)
            .map(|binding| binding.operation);
        let root_stream = root_stream_name(callee);
        let root_stream_operation = match (root_stream, input.as_ref()) {
            (Some("from_list"), None) => Some("from_list"),
            (Some("for_each"), Some(Value::Stream { .. })) => Some("for_each"),
            _ => None,
        };
        let native_hash = native_binding
            .filter(|binding| binding.kind == StandardBindingKind::Hash)
            .map(|binding| binding.operation);
        let native_random = native_binding
            .filter(|binding| binding.kind == StandardBindingKind::Random)
            .map(|binding| binding.operation);
        let native_base64 = native_binding
            .filter(|binding| binding.kind == StandardBindingKind::Base64)
            .map(|binding| binding.operation);
        let native_json = native_binding
            .filter(|binding| binding.kind == StandardBindingKind::Json)
            .map(|binding| binding.operation);
        let native_orna_codec = native_binding
            .filter(|binding| binding.kind == StandardBindingKind::OrnaCodec)
            .map(|binding| binding.operation);
        let native_ovb_codec = native_binding
            .filter(|binding| binding.kind == StandardBindingKind::OvbCodec)
            .map(|binding| binding.operation);
        let native_money = native_binding
            .filter(|binding| binding.kind == StandardBindingKind::Money)
            .map(|binding| binding.operation);
        let qualified_math = (!scope.0.contains_key("std"))
            .then(|| math_name(callee))
            .flatten();
        let qualified_text = (!scope.0.contains_key("std"))
            .then(|| text_name(callee))
            .flatten();
        let qualified_bits = (!scope.0.contains_key("std"))
            .then(|| bits_name(callee))
            .flatten();
        let qualified_stats = (!scope.0.contains_key("std"))
            .then(|| stats_name(callee))
            .flatten();
        let qualified_time = (!scope.0.contains_key("std"))
            .then(|| time_name(callee))
            .flatten();
        if self.restrict_function_names
            && resolved_function.is_none()
            && ((qualified_math.is_some() && native_math.is_none())
                || (qualified_text.is_some() && native_text.is_none())
                || (qualified_bits.is_some() && native_bits.is_none())
                || (qualified_stats.is_some() && native_stats.is_none())
                || (qualified_time.is_some() && native_time.is_none()))
        {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        let native_collection = native_binding.is_some_and(|binding| {
            binding.kind == StandardBindingKind::Collection
                && (binding.operation.starts_with("__") || binding.operation == "map")
        });
        let native_asof_join = portable_collection_name(callee) == Some("asof_join")
            && !self.restrict_function_names
            && !scope.0.contains_key("std")
            && resolved_function.is_none();
        if portable_collection_operation(callee, resolved_function.as_deref()).is_some()
            && self.restrict_function_names
            && resolved_function.is_none()
            && !native_asof_join
            && !native_collection
        {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        // Portable collection/query exports and selected exact arithmetic
        // leaves are admitted by their captured source declarations. Public
        // functions execute their Orna bodies; `map` and private leaves use
        // bounded primitives so callbacks retain the evaluator's captured
        // namespace and module-pin context.
        if !native_asof_join
            && !native_collection
            && root_stream_operation.is_none()
            && native_math.is_none()
            && native_text.is_none()
            && native_bits.is_none()
            && native_stream.is_none()
            && native_concurrent.is_none()
            && native_stats.is_none()
            && native_time.is_none()
            && native_hash.is_none()
            && native_random.is_none()
            && native_base64.is_none()
            && native_json.is_none()
            && native_orna_codec.is_none()
            && native_ovb_codec.is_none()
            && native_money.is_none()
            && native_ui.is_none()
            && (qualified_math.is_none()
                && qualified_bits.is_none()
                && qualified_text.is_none()
                && qualified_stats.is_none()
                && qualified_time.is_none()
                && native_stream.is_none()
                && native_concurrent.is_none()
                && portable_collection_operation(callee, resolved_function.as_deref()).is_none()
                && root_collection.is_none()
                || resolved_function.is_some())
        {
            // Only an unresolved, statically rooted field path is an effect
            // dispatch candidate. Dynamic field callees must be evaluated by
            // the ordinary call path first, otherwise their argument effects
            // would run before the callee (for example,
            // `make().field(effectful_arg)`).
            let static_effect_path = is_static_effect_path(callee, scope);
            if static_effect_path && self.effects.is_some() {
                self.items(arguments.len() + usize::from(input.is_some()))?;
                let mut values = input.clone().into_iter().collect::<Vec<_>>();
                for argument in arguments {
                    values.push(self.evaluate(&argument.value, scope, depth + 1)?);
                    if self.transfer.is_some() {
                        return Ok(Value::Null);
                    }
                }
                let values = values
                    .into_iter()
                    .map(Value::canonical)
                    .collect::<Result<Vec<_>, _>>()?;
                let remaining = self.limits.max_steps.saturating_sub(self.steps);
                let mut budget = StepBudget::new(remaining);
                let result = self
                    .effects
                    .as_deref_mut()
                    .expect("checked effect handler")
                    .handle_with_budget(callee, &values, &mut budget);
                let debited = remaining - budget.remaining();
                self.steps = self
                    .steps
                    .checked_add(debited)
                    .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
                let handled = result?;
                if let Some(value) = handled {
                    return self.effect_value(&value);
                }
            }
            if self.reject_unhandled_field_calls
                && matches!(callee, Expr::Field { .. })
                && self.resolve_function_name(callee, scope).is_none()
            {
                return Err(error("ORNA-EVAL-UNSUPPORTED"));
            }
            // A qualified module function is a callable path, not a record
            // field lookup. Resolve it only when the complete path was
            // explicitly admitted in the function environment; ordinary
            // record fields retain their existing semantics.
            let callable = if let Some(name) = self.resolve_function_name(callee, scope) {
                Value::Function {
                    name,
                    nominal_definitions: scope.3.clone(),
                }
            } else {
                self.evaluate(callee, scope, depth + 1)?
            };
            if self.transfer.is_some() {
                return Ok(Value::Null);
            }
            let functions = self.functions;
            let (parameters, body, mut captured, session_owned, namespace) = match &callable {
                Value::Function {
                    name,
                    nominal_definitions,
                } => {
                    let function = functions.get(name).ok_or_else(|| error("ORNA-EVAL-NAME"))?;
                    (
                        &function.parameters,
                        &function.body,
                        Scope::from_environment_with_nominals(
                            &function.environment,
                            nominal_definitions,
                            self,
                        )?,
                        self.session_functions
                            .is_some_and(|functions| functions.contains(name)),
                        function_namespace(name),
                    )
                }
                Value::Closure(closure) => (
                    &closure.parameters,
                    &closure.body,
                    closure.captured.clone(),
                    self.repl_bindings,
                    closure.namespace.clone(),
                ),
                _ => return Err(error("ORNA-EVAL-TYPE")),
            };
            captured.2.extend(
                scope
                    .2
                    .iter()
                    .filter(|name| captured.0.contains_key(*name))
                    .cloned(),
            );
            for (name, definition) in &scope.3 {
                captured
                    .3
                    .entry(name.clone())
                    .or_insert_with(|| definition.clone());
            }
            self.depth(depth + 1)?;
            self.items(arguments.len() + usize::from(input.is_some()))?;
            let mut supplied = BTreeMap::new();
            let mut positional = 0;
            if let Some(input) = input {
                let Some(parameter) = parameters.first() else {
                    return Err(error("ORNA-EVAL-ARGUMENT"));
                };
                supplied.insert(parameter_key(parameter, 0), input);
                positional = 1;
            }
            let mut named_started = false;
            for argument in arguments {
                let key = if let Some(name) = &argument.name {
                    named_started = true;
                    ArgumentKey::Name(name.clone())
                } else {
                    if named_started {
                        return Err(error("ORNA-EVAL-ARGUMENT"));
                    }
                    let Some(parameter) = parameters.get(positional) else {
                        return Err(error("ORNA-EVAL-ARGUMENT"));
                    };
                    let key = parameter_key(parameter, positional);
                    positional += 1;
                    key
                };
                if supplied.contains_key(&key) {
                    return Err(error("ORNA-EVAL-ARGUMENT"));
                }
                let value = self.evaluate(&argument.value, scope, depth + 1)?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                supplied.insert(key, value);
            }
            let previous_repl_bindings = self.repl_bindings;
            self.repl_bindings = session_owned;
            let previous_namespace = self.namespace.clone();
            self.namespace = namespace;
            let result = invoke_pure(self, parameters, body, captured, supplied, depth + 1);
            self.namespace = previous_namespace;
            self.repl_bindings = previous_repl_bindings;
            return result;
        }
        let math = native_math.or_else(|| {
            (!self.restrict_function_names)
                .then_some(qualified_math)
                .flatten()
        });
        let bits = native_bits.or_else(|| {
            (!self.restrict_function_names)
                .then_some(qualified_bits)
                .flatten()
        });
        let text = native_text.or_else(|| {
            (!self.restrict_function_names)
                .then_some(qualified_text)
                .flatten()
        });
        let stats = native_stats.or_else(|| {
            (!self.restrict_function_names)
                .then_some(qualified_stats)
                .flatten()
        });
        let time = native_time;
        let stream = native_stream.or(root_stream_operation);
        let concurrent = native_concurrent;
        let base64 = native_base64;
        let json = native_json;
        let orna_codec = native_orna_codec;
        let ovb_codec = native_ovb_codec;
        let money = native_money;
        let collection =
            portable_collection_operation(callee, resolved_function.as_deref()).or(root_collection);
        let name = math
            .or(bits)
            .or(text)
            .or(stats)
            .or(time)
            .or(stream)
            .or(concurrent)
            .or(native_ui)
            .or(native_hash)
            .or(native_random)
            .or(base64)
            .or(json)
            .or(orna_codec)
            .or(ovb_codec)
            .or(money)
            .or(collection)
            .ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?;
        let implicit = usize::from(input.is_some());
        self.items(arguments.len() + implicit)?;
        let mut values = input.into_iter().collect::<Vec<_>>();
        let mut explicit = Vec::with_capacity(arguments.len());
        for argument in arguments {
            let shorthand = stats.is_some()
                && argument
                    .name
                    .as_deref()
                    .is_some_and(|name| matches!(name, "rounding" | "interpolation" | "method"))
                && matches!(
                    argument.value,
                    Expr::Name { ref text, .. } if matches!(text.as_str(), "half_even" | "linear" | "lower" | "higher" | "nearest" | "midpoint")
                );
            explicit.push(if shorthand {
                let Expr::Name { text, .. } = &argument.value else {
                    unreachable!("stats shorthand was checked above");
                };
                Value::String(text.clone())
            } else {
                self.evaluate(&argument.value, scope, depth + 1)?
            });
            if self.transfer.is_some() {
                return Ok(Value::Null);
            }
        }
        values.extend(explicit);
        let binding_name = match native_binding.map(|binding| binding.kind) {
            Some(StandardBindingKind::Hash) => format!("hash.{name}"),
            Some(StandardBindingKind::Random) => {
                format!("random.{}", name.strip_prefix("__").unwrap_or(name))
            }
            Some(StandardBindingKind::Base64) => {
                format!("base64.{}", name.strip_prefix("__").unwrap_or(name))
            }
            Some(StandardBindingKind::Money) => {
                format!("money.{}", name.strip_prefix("__").unwrap_or(name))
            }
            _ => name.to_owned(),
        };
        let values = named_arguments(
            &binding_name,
            arguments,
            values,
            implicit,
            collection.is_some() || stream.is_some(),
        )?;
        if math.is_some() {
            self.math(name, values)
        } else if bits.is_some() {
            self.bits(name, values)
        } else if text.is_some() {
            self.text(name, values)
        } else if stats.is_some() {
            let value = self.stats(name, values)?;
            let nullable = matches!(
                name,
                "__mean"
                    | "__median"
                    | "__percentile"
                    | "__min"
                    | "__max"
                    | "__range"
                    | "__variance"
                    | "__standard_deviation"
                    | "__rate"
                    | "__integrate"
            );
            if self.restrict_function_names
                && nullable
                && !matches!(value, Value::Null | Value::Option(_))
            {
                Ok(Value::Option(Some(Box::new(value))))
            } else {
                Ok(value)
            }
        } else if time.is_some() {
            self.time(name, values)
        } else if stream.is_some() {
            self.stream(name, values, depth)
        } else if concurrent.is_some() {
            self.concurrent(name, values, depth)
        } else if native_ui.is_some() {
            self.ui_node(values)
        } else if native_hash.is_some() {
            self.hash(name, values)
        } else if native_random.is_some() {
            self.random(name, values, callee)
        } else if base64.is_some() {
            self.base64(name, values)
        } else if json.is_some() {
            self.json_codec(name, values)
        } else if orna_codec.is_some() {
            self.orna_codec(name, values)
        } else if ovb_codec.is_some() {
            self.ovb_codec(name, values)
        } else if money.is_some() {
            self.money(name, values)
        } else {
            self.collection(name, values, depth)
        }
    }
    fn resolve_function_name(&self, expression: &Expr, scope: &Scope) -> Option<String> {
        let name = function_name(expression)?;
        if let Some(root) = function_root_name(expression)
            && scope.0.contains_key(root)
            && !scope.2.contains(root)
        {
            return None;
        }
        if let Some(namespace) = self.namespace.as_deref() {
            if !name.contains('.') {
                let qualified = format!("{namespace}.{name}");
                if self.functions.contains_key(&qualified) {
                    return Some(qualified);
                }
            } else if self.functions.contains_key(&name) {
                return Some(name);
            }
            let module_alias = module_function_alias_key(namespace, &name);
            if let Some(alias) = self.aliases.and_then(|aliases| aliases.get(&module_alias))
                && self.functions.contains_key(alias)
            {
                return Some(alias.clone());
            }
        }
        if let Some(alias) = self.aliases.and_then(|aliases| aliases.get(&name))
            && self.functions.contains_key(alias)
        {
            return Some(alias.clone());
        }
        if self.functions.contains_key(&name)
            && orna_sys_v1::system_host_operation_registry()
                .operation(&name)
                .is_some()
        {
            return Some(name);
        }
        // Captured std modules are admitted source. Resolve their exact
        // qualified identities in the bounded REPL so public Orna wrappers
        // execute before private bounded primitives are dispatched.
        if name.starts_with("std.") && self.functions.contains_key(&name) {
            return Some(name);
        }
        if !self.restrict_function_names && self.functions.contains_key(&name) {
            return Some(name);
        }
        if name.contains('.') {
            return None;
        }
        let namespace = self.namespace.as_deref()?;
        let qualified = format!("{namespace}.{name}");
        self.functions.contains_key(&qualified).then_some(qualified)
    }
    fn index(&self, base: Value, index: Value) -> Result<Value, EvaluationError> {
        let Value::Int(index) = index else {
            return Err(error("ORNA-EVAL-TYPE"));
        };
        let index = index.to_usize().ok_or_else(|| error("ORNA-EVAL-INDEX"))?;
        match base {
            Value::List(values) | Value::Tuple(values) => values
                .get(index)
                .cloned()
                .ok_or_else(|| error("ORNA-EVAL-INDEX")),
            _ => Err(error("ORNA-EVAL-TYPE")),
        }
    }
    fn case(
        &mut self,
        condition: &Expr,
        arms: &[orna_syntax_v1::CaseArm],
        scope: &mut Scope,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        let value = self.evaluate(condition, scope, depth + 1)?;
        if self.transfer.is_some() {
            return Ok(Value::Null);
        }
        self.items(arms.len())?;
        for arm in arms {
            let mut local = scope.clone();
            if !bind(&arm.pattern, value.clone(), &mut local, self, depth + 1)? {
                continue;
            }
            if let Some(guard) = &arm.guard {
                match self.evaluate(guard, &mut local, depth + 1)? {
                    _ if self.transfer.is_some() => return Ok(Value::Null),
                    Value::Bool(true) => {}
                    Value::Bool(false) => continue,
                    _ => return Err(error("ORNA-EVAL-TYPE")),
                }
            }
            return self.evaluate(&arm.body, &mut local, depth + 1);
        }
        Err(error("ORNA-EVAL-NO-MATCH"))
    }
    fn math(&self, name: &str, values: Vec<Value>) -> Result<Value, EvaluationError> {
        match (name, values.as_slice()) {
            ("increment", [value]) => self.apply_binary("+", value.clone(), one_like(value)?),
            ("decrement", [value]) => self.apply_binary("-", value.clone(), one_like(value)?),
            ("is_zero", [Value::Int(value)]) => Ok(Value::Bool(value.is_zero())),
            ("is_zero", [Value::Decimal(value)]) => Ok(Value::Bool(value.coefficient.is_zero())),
            ("is_zero", [Value::Float(value)]) => Ok(Value::Bool(f64::from_bits(*value) == 0.0)),
            ("min", [a, b]) => ordered(a, b, true),
            ("max", [a, b]) => ordered(a, b, false),
            ("clamp", [value, low, high]) => {
                if compare_values(low, high)? == std::cmp::Ordering::Greater {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                if compare_values(value, low)? == std::cmp::Ordering::Less {
                    Ok(low.clone())
                } else if compare_values(value, high)? == std::cmp::Ordering::Greater {
                    Ok(high.clone())
                } else {
                    Ok(value.clone())
                }
            }
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn bits(&self, name: &str, values: Vec<Value>) -> Result<Value, EvaluationError> {
        let name = name.strip_prefix("__").unwrap_or(name);
        match (name, values.as_slice()) {
            ("bit_or", [Value::Int(left), Value::Int(right)]) => {
                Ok(Value::Int(self.integer(left | right)?))
            }
            ("bit_and", [Value::Int(left), Value::Int(right)]) => {
                Ok(Value::Int(self.integer(left & right)?))
            }
            ("bit_xor", [Value::Int(left), Value::Int(right)]) => {
                Ok(Value::Int(self.integer(left ^ right)?))
            }
            ("bit_not", [Value::Int(value)]) => {
                Ok(Value::Int(self.integer(-value - BigInt::from(1))?))
            }
            ("shift_left", [Value::Int(value), Value::Int(count)]) => self.shift_left(value, count),
            ("shift_right", [Value::Int(value), Value::Int(count)]) => {
                self.shift_right(value, count)
            }
            ("bit_or" | "bit_and" | "bit_xor" | "bit_not" | "shift_left" | "shift_right", _)
                if values.iter().any(|value| !matches!(value, Value::Int(_))) =>
            {
                Err(error("ORNA-EVAL-TYPE"))
            }
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn text(&mut self, name: &str, values: Vec<Value>) -> Result<Value, EvaluationError> {
        let name = name.strip_prefix("__").unwrap_or(name);
        match (name, values.as_slice()) {
            ("trim", [Value::String(value)]) => {
                // Keep the pinned standard profile independent of the Rust
                // toolchain's Unicode tables. Unicode 16.0.0's White_Space
                // property is stable and explicitly listed above.
                self.string(value.trim_matches(unicode_16_white_space).to_owned())
                    .map(Value::String)
            }
            ("split", [Value::String(value), Value::String(separator)]) => {
                // This profile preserves leading/trailing empty fields. An
                // empty separator iterates Unicode scalar values, as required
                // by the portable std.text contract.
                let count = if separator.is_empty() {
                    value.chars().count()
                } else {
                    value.split(separator).count()
                };
                self.items(count)?;
                let mut fields = Vec::with_capacity(count);
                if separator.is_empty() {
                    for scalar in value.chars() {
                        self.step()?;
                        fields.push(Value::String(scalar.to_string()));
                    }
                } else {
                    for field in value.split(separator) {
                        self.step()?;
                        fields.push(Value::String(field.to_owned()));
                    }
                }
                Ok(Value::List(fields))
            }
            ("join", [Value::List(values), Value::String(separator)]) => {
                self.items(values.len())?;
                let mut output = String::new();
                for (index, value) in values.iter().enumerate() {
                    let Value::String(value) = value else {
                        return Err(error("ORNA-EVAL-TYPE"));
                    };
                    self.step()?;
                    if index > 0 {
                        output.push_str(separator);
                    }
                    output.push_str(value);
                    if output.len() > self.limits.max_string_bytes {
                        return Err(error("ORNA-EVAL-LIMIT"));
                    }
                }
                Ok(Value::String(output))
            }
            ("starts_with", [Value::String(value), Value::String(prefix)]) => {
                Ok(Value::Bool(value.starts_with(prefix)))
            }
            ("ends_with", [Value::String(value), Value::String(suffix)]) => {
                Ok(Value::Bool(value.ends_with(suffix)))
            }
            ("contains", [Value::String(value), Value::String(needle)]) => {
                Ok(Value::Bool(value.contains(needle)))
            }
            ("replace", [Value::String(value), Value::String(from), Value::String(to)]) => {
                // `str::replace` is non-overlapping and left-to-right.
                self.string(value.replace(from, to)).map(Value::String)
            }
            ("normalise", [Value::String(value), Value::String(form)]) => match form.as_str() {
                // The reference leaves the form labels open; expose the two
                // canonical forms directly and fail closed for other labels.
                "NFC" => self.string(value.nfc().collect()).map(Value::String),
                "NFD" => self.string(value.nfd().collect()).map(Value::String),
                _ => Err(error("ORNA-EVAL-VALUE")),
            },
            ("lower", [Value::String(value)]) => {
                self.string(unicode_16_lowercase(value)).map(Value::String)
            }
            ("upper", [Value::String(value)]) => {
                self.string(unicode_16_uppercase(value)).map(Value::String)
            }
            ("trim" | "lower" | "upper", [_])
            | ("split" | "starts_with" | "ends_with" | "contains", [_, _])
            | ("join", [_, _])
            | ("replace", [_, _, _])
            | ("normalise", [_, _]) => Err(error("ORNA-EVAL-TYPE")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn checked_stream_values(
        &mut self,
        stream: &Value,
    ) -> Result<(Vec<Value>, String), EvaluationError> {
        let Value::Stream {
            values,
            source_label,
            source_digest,
            position,
            provider,
        } = stream
        else {
            return Err(error("ORNA-EVAL-TYPE"));
        };
        if provider.is_some()
            || *position > values.len()
            || self.list_stream_digest(source_label, values)? != *source_digest
        {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        self.items(values.len().saturating_sub(*position))?;
        Ok((values[*position..].to_vec(), source_label.clone()))
    }

    fn finite_stream(
        &mut self,
        source_label: String,
        values: Vec<Value>,
    ) -> Result<Value, EvaluationError> {
        self.string(source_label.clone())?;
        self.items(values.len())?;
        let source_digest = self.list_stream_digest(&source_label, &values)?;
        Ok(Value::Stream {
            values,
            source_label,
            source_digest,
            position: 0,
            provider: None,
        })
    }

    fn stream_effect<T>(
        &mut self,
        operation: impl FnOnce(
            &mut dyn EffectHandler,
            &mut StepBudget,
            Option<&CancellationToken>,
        ) -> Result<T, EvaluationError>,
    ) -> Result<T, EvaluationError> {
        let remaining = self.limits.max_steps.saturating_sub(self.steps);
        let mut budget = StepBudget::new(remaining);
        let cancellation = self.cancellation;
        let result = match self.effects.as_deref_mut() {
            Some(effects) => operation(effects, &mut budget, cancellation),
            None => Err(error("ORNA-EVAL-UNSUPPORTED")),
        };
        let debited = remaining - budget.remaining();
        self.steps = self
            .steps
            .checked_add(debited)
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        result
    }

    fn provider_stream(
        &mut self,
        source: &str,
        identity_label: &str,
    ) -> Result<Value, EvaluationError> {
        let source = self.string(source.to_owned())?;
        let identity_label = self.string(identity_label.to_owned())?;
        if source.is_empty() || identity_label.is_empty() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        // The reference pins finite-list identities, not connector identities.
        // Bind the live cursor to both provider source and consumer label.
        let identity_value = Raw::Array(vec![
            Raw::Text(source.clone()),
            Raw::Text(identity_label.clone()),
        ]);
        let identity = domain_digest("orna.provider-stream.v1", &identity_value)
            .map_err(|_| error("ORNA-EVAL-VALUE"))?;
        let available = self.stream_effect(|effects, budget, cancellation| {
            effects.validate_stream_source(&source, &identity, budget, cancellation)
        })?;
        if !available {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        let source_label = format!("{source}:{identity_label}");
        self.string(source_label.clone())?;
        Ok(Value::Stream {
            values: Vec::new(),
            source_label,
            source_digest: identity,
            position: 0,
            provider: Some(ProviderStream {
                sources: vec![StreamSourceCursor {
                    source,
                    identity,
                    after: None,
                }],
                buffer_capacity: 1,
                batch_size: None,
                throttle_nanoseconds: None,
                debounce_nanoseconds: None,
                retry: None,
                recover: None,
            }),
        })
    }

    fn provider_stream_value(
        &mut self,
        source_label: String,
        source_digest: [u8; 32],
        provider: ProviderStream,
    ) -> Result<Value, EvaluationError> {
        self.string(source_label.clone())?;
        self.items(provider.sources.len())?;
        Ok(Value::Stream {
            values: Vec::new(),
            source_label,
            source_digest,
            position: 0,
            provider: Some(provider),
        })
    }

    fn stream_checkpoints(items: &[ReadyStreamItem]) -> Vec<StreamCheckpoint> {
        let mut checkpoints = BTreeMap::<(String, [u8; 32]), Vec<u8>>::new();
        for item in items {
            checkpoints
                .entry((item.source.clone(), item.identity))
                .and_modify(|checkpoint| {
                    if item.checkpoint > *checkpoint {
                        *checkpoint = item.checkpoint.clone();
                    }
                })
                .or_insert_with(|| item.checkpoint.clone());
        }
        checkpoints
            .into_iter()
            .map(|((source, identity), checkpoint)| StreamCheckpoint {
                source,
                identity,
                checkpoint,
            })
            .collect()
    }

    fn commit_stream_checkpoints(
        &mut self,
        checkpoints: &[StreamCheckpoint],
    ) -> Result<(), EvaluationError> {
        if checkpoints.is_empty() {
            return Ok(());
        }
        self.stream_effect(|effects, budget, cancellation| {
            effects.begin_stream_delivery(checkpoints, budget, cancellation)
        })?;
        let commit = self.stream_effect(|effects, budget, cancellation| {
            effects.commit_stream_delivery(checkpoints, budget, cancellation)
        });
        if let Err(failure) = commit {
            let _ = self.stream_effect(|effects, budget, _| {
                effects.rollback_stream_delivery(checkpoints, budget, None)
            });
            return Err(failure);
        }
        Ok(())
    }

    fn rollback_stream_checkpoints(&mut self, checkpoints: &[StreamCheckpoint]) {
        let _ = self.stream_effect(|effects, budget, _| {
            effects.rollback_stream_delivery(checkpoints, budget, None)
        });
    }

    fn commit_stream_callback(
        &mut self,
        items: &[ReadyStreamItem],
        action: &Value,
        recovery: Option<&Value>,
        batch_size: Option<usize>,
        depth: usize,
    ) -> Result<(), EvaluationError> {
        if items.is_empty() {
            return Ok(());
        }
        let checkpoints = Self::stream_checkpoints(items);
        self.stream_effect(|effects, budget, cancellation| {
            effects.begin_stream_delivery(&checkpoints, budget, cancellation)
        })?;

        let callback = (|| {
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                let value = if let Some(failure) = &item.recovery_error {
                    let recovery = recovery.ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                    self.invoke_predicate(recovery, Value::Error(failure.clone()), depth + 1)?
                } else {
                    item.value.clone()
                };
                values.push(value);
            }
            let argument = if batch_size.is_some() {
                Value::List(values)
            } else {
                values
                    .into_iter()
                    .next()
                    .ok_or_else(|| error("ORNA-EVAL-VALUE"))?
            };
            let result = self.invoke_predicate(action, argument, depth + 1)?;
            if !matches!(result, Value::Unit | Value::Null) {
                return Err(error("ORNA-EVAL-TYPE"));
            }
            self.step()
        })();

        if let Err(failure) = callback {
            self.rollback_stream_checkpoints(&checkpoints);
            return Err(failure);
        }
        let commit = self.stream_effect(|effects, budget, cancellation| {
            effects.commit_stream_delivery(&checkpoints, budget, cancellation)
        });
        if let Err(failure) = commit {
            self.rollback_stream_checkpoints(&checkpoints);
            return Err(failure);
        }
        Ok(())
    }

    fn wait_stream_retry_delay(&mut self, nanoseconds: &BigInt) -> Result<(), EvaluationError> {
        let mut remaining = nanoseconds.clone();
        let quantum = BigInt::from(10_000_000u32);
        while remaining > BigInt::from(0u8) {
            self.step()?;
            let wait = remaining.clone().min(quantum.clone());
            let nanos = wait.to_u32().ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
            std::thread::sleep(std::time::Duration::from_nanos(u64::from(nanos)));
            remaining -= wait;
        }
        Ok(())
    }

    fn recoverable_stream_item(
        failure: StreamFailure,
        recovery_enabled: bool,
    ) -> Result<ReadyStreamItem, EvaluationError> {
        let Some(checkpoint) = failure.checkpoint else {
            return Err(failure.error);
        };
        if !failure.recoverable || !recovery_enabled {
            return Err(failure.error);
        }
        Ok(ReadyStreamItem {
            source: failure.source,
            identity: failure.identity,
            checkpoint,
            event_time: failure.event_time,
            value: Value::Error(failure.error.clone()),
            recovery_error: Some(failure.error),
        })
    }

    fn retry_provider_delivery(
        &mut self,
        provider: &ProviderStream,
        cursor: &StreamSourceCursor,
        mut failure: StreamFailure,
        watermark: &mut Instant,
    ) -> Result<ReadyStreamItem, EvaluationError> {
        if let Some(retry) = &provider.retry {
            let expected_checkpoint = failure
                .checkpoint
                .as_deref()
                .ok_or_else(|| failure.error.clone())?
                .to_vec();
            let mut delay = retry.initial_delay_nanoseconds.clone();
            for _ in 1..retry.max_attempts {
                self.wait_stream_retry_delay(&delay)?;
                let mut source_ended = false;
                loop {
                    self.step()?;
                    let page = self.stream_effect(|effects, budget, cancellation| {
                        effects.poll_stream_sources(
                            std::slice::from_ref(cursor),
                            1,
                            budget,
                            cancellation,
                        )
                    })?;
                    if page.events.len() > 1
                        || page.watermark < *watermark
                        || (page.events.is_empty()
                            && page.ended_sources.is_empty()
                            && page.watermark <= *watermark)
                        || page
                            .ended_sources
                            .iter()
                            .any(|source| source != &cursor.source)
                    {
                        return Err(error("ORNA-EVAL-VALUE"));
                    }
                    *watermark = page.watermark;
                    let Some(event) = page.events.into_iter().next() else {
                        if page.ended_sources.contains(&cursor.source) {
                            source_ended = true;
                            break;
                        }
                        continue;
                    };
                    match event {
                        StreamEvent::Delivery(delivery) => {
                            if delivery.source != cursor.source
                                || delivery.identity != cursor.identity
                                || delivery.checkpoint != expected_checkpoint
                                || delivery.event_time != failure.event_time
                                || delivery.event_time > *watermark
                            {
                                return Err(error("ORNA-EVAL-VALUE"));
                            }
                            return Ok(ReadyStreamItem {
                                source: delivery.source,
                                identity: delivery.identity,
                                checkpoint: delivery.checkpoint,
                                event_time: delivery.event_time,
                                value: Value::from_canonical(&delivery.value, self, 0)?,
                                recovery_error: None,
                            });
                        }
                        StreamEvent::Failure(next) => {
                            if next.source != cursor.source
                                || next.identity != cursor.identity
                                || next.checkpoint.as_deref() != Some(&expected_checkpoint)
                                || next.event_time != failure.event_time
                                || next.event_time > *watermark
                            {
                                return Err(error("ORNA-EVAL-VALUE"));
                            }
                            failure = next;
                            break;
                        }
                    }
                }
                if source_ended {
                    break;
                }
                delay = (delay * BigInt::from(2u8)).min(retry.max_delay_nanoseconds.clone());
            }
        }
        Self::recoverable_stream_item(failure, provider.recover.is_some())
    }

    fn consume_provider_stream(
        &mut self,
        stream: &Value,
        action: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        let Value::Stream {
            provider: Some(provider),
            ..
        } = stream
        else {
            return Err(error("ORNA-EVAL-TYPE"));
        };
        self.depth(depth)?;
        self.items(provider.sources.len())?;
        let mut cursors = provider.sources.clone();
        let mut source_names = BTreeSet::new();
        for cursor in &cursors {
            if cursor.source.is_empty() || !source_names.insert(cursor.source.clone()) {
                return Err(error("ORNA-EVAL-VALUE"));
            }
        }
        let mut ended = BTreeSet::<String>::new();
        let mut last_watermark = None;
        let mut last_throttle_event = None;
        let mut debounced = None::<ReadyStreamItem>;
        let mut batch = Vec::<ReadyStreamItem>::new();

        loop {
            let active = cursors
                .iter()
                .filter(|cursor| !ended.contains(&cursor.source))
                .cloned()
                .collect::<Vec<_>>();
            if active.is_empty() {
                if let Some(item) = debounced.take() {
                    batch.push(item);
                }
                if !batch.is_empty() {
                    self.commit_stream_callback(
                        &batch,
                        action,
                        provider.recover.as_deref(),
                        provider.batch_size,
                        depth,
                    )?;
                    batch.clear();
                }
                break;
            }

            self.step()?;
            let page = self.stream_effect(|effects, budget, cancellation| {
                effects.poll_stream_sources(&active, provider.buffer_capacity, budget, cancellation)
            })?;
            if page.events.len() > provider.buffer_capacity
                || last_watermark.is_some_and(|last| page.watermark < last)
                || (page.events.is_empty()
                    && page.ended_sources.is_empty()
                    && last_watermark.is_some_and(|last| page.watermark <= last))
                || page
                    .ended_sources
                    .iter()
                    .any(|source| !active.iter().any(|cursor| &cursor.source == source))
            {
                return Err(error("ORNA-EVAL-VALUE"));
            }

            // Validate the entire bounded page before executing any callback
            // from it, so malformed later events cannot follow committed work.
            let mut observed = cursors
                .iter()
                .map(|cursor| {
                    (
                        (cursor.source.clone(), cursor.identity),
                        cursor.after.clone(),
                    )
                })
                .collect::<BTreeMap<_, _>>();
            let mut blocked = BTreeSet::<(String, [u8; 32])>::new();
            for event in &page.events {
                let (source, identity, checkpoint, event_time) = match event {
                    StreamEvent::Delivery(delivery) => (
                        &delivery.source,
                        delivery.identity,
                        Some(&delivery.checkpoint),
                        delivery.event_time,
                    ),
                    StreamEvent::Failure(failure) => (
                        &failure.source,
                        failure.identity,
                        failure.checkpoint.as_ref(),
                        failure.event_time,
                    ),
                };
                let key = (source.clone(), identity);
                let Some(previous) = observed.get_mut(&key) else {
                    return Err(error("ORNA-EVAL-VALUE"));
                };
                if event_time > page.watermark || blocked.contains(&key) {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                if let Some(checkpoint) = checkpoint {
                    if previous
                        .as_ref()
                        .is_some_and(|previous| checkpoint <= previous)
                    {
                        return Err(error("ORNA-EVAL-VALUE"));
                    }
                    *previous = Some(checkpoint.clone());
                }
                if matches!(event, StreamEvent::Failure(_)) {
                    blocked.insert(key);
                }
            }
            last_watermark = Some(page.watermark);

            for event in page.events {
                let ready = match event {
                    StreamEvent::Delivery(delivery) => {
                        let Some(cursor) = cursors.iter_mut().find(|cursor| {
                            cursor.source == delivery.source && cursor.identity == delivery.identity
                        }) else {
                            return Err(error("ORNA-EVAL-VALUE"));
                        };
                        cursor.after = Some(delivery.checkpoint.clone());
                        ReadyStreamItem {
                            source: delivery.source,
                            identity: delivery.identity,
                            checkpoint: delivery.checkpoint,
                            event_time: delivery.event_time,
                            value: Value::from_canonical(&delivery.value, self, 0)?,
                            recovery_error: None,
                        }
                    }
                    StreamEvent::Failure(failure) => {
                        let cursor = cursors
                            .iter()
                            .find(|cursor| {
                                cursor.source == failure.source
                                    && cursor.identity == failure.identity
                            })
                            .cloned()
                            .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                        if let Some(previous) = debounced.take() {
                            batch.push(previous);
                        }
                        if !batch.is_empty() {
                            self.commit_stream_callback(
                                &batch,
                                action,
                                provider.recover.as_deref(),
                                provider.batch_size,
                                depth,
                            )?;
                            batch.clear();
                        }
                        self.retry_provider_delivery(
                            provider,
                            &cursor,
                            failure,
                            last_watermark
                                .as_mut()
                                .expect("a validated page has a watermark"),
                        )?
                    }
                };

                if let (Some(interval), Some(pending)) =
                    (&provider.debounce_nanoseconds, debounced.as_ref())
                    && stream_elapsed_nanoseconds(ready.event_time, pending.event_time)
                        .is_some_and(|elapsed| elapsed >= *interval)
                {
                    let expired = debounced.take().expect("pending debounce value exists");
                    batch.push(expired);
                    let target = provider.batch_size.unwrap_or(1);
                    if batch.len() >= target {
                        self.commit_stream_callback(
                            &batch,
                            action,
                            provider.recover.as_deref(),
                            provider.batch_size,
                            depth,
                        )?;
                        batch.clear();
                    }
                }

                if provider.debounce_nanoseconds.is_some() {
                    // `latest` may acknowledge a replaced item only after all
                    // older buffered items from that same source have run.
                    if let Some(previous) = debounced.take() {
                        let checkpoints = Self::stream_checkpoints(std::slice::from_ref(&previous));
                        if batch.iter().any(|item| {
                            item.source == previous.source && item.identity == previous.identity
                        }) {
                            self.commit_stream_callback(
                                &batch,
                                action,
                                provider.recover.as_deref(),
                                provider.batch_size,
                                depth,
                            )?;
                            batch.clear();
                        }
                        self.commit_stream_checkpoints(&checkpoints)?;
                    }
                    debounced = Some(ready);
                } else if let Some(interval) = &provider.throttle_nanoseconds {
                    // Equal event-time ties retain provider arrival order: the
                    // first eligible value wins and every drop is checkpointed.
                    let emit = last_throttle_event.is_none_or(|last| {
                        stream_elapsed_nanoseconds(ready.event_time, last)
                            .is_some_and(|elapsed| elapsed >= *interval)
                    });
                    if emit {
                        last_throttle_event = Some(ready.event_time);
                        batch.push(ready);
                    } else {
                        let checkpoints = Self::stream_checkpoints(std::slice::from_ref(&ready));
                        if batch.iter().any(|item| {
                            item.source == ready.source && item.identity == ready.identity
                        }) {
                            self.commit_stream_callback(
                                &batch,
                                action,
                                provider.recover.as_deref(),
                                provider.batch_size,
                                depth,
                            )?;
                            batch.clear();
                        }
                        self.commit_stream_checkpoints(&checkpoints)?;
                    }
                } else {
                    batch.push(ready);
                }

                let target = provider.batch_size.unwrap_or(1);
                if batch.len() >= target {
                    self.commit_stream_callback(
                        &batch,
                        action,
                        provider.recover.as_deref(),
                        provider.batch_size,
                        depth,
                    )?;
                    batch.clear();
                }
            }

            if let (Some(interval), Some(pending)) =
                (&provider.debounce_nanoseconds, debounced.as_ref())
                && stream_elapsed_nanoseconds(page.watermark, pending.event_time)
                    .is_some_and(|elapsed| elapsed >= *interval)
            {
                batch.push(debounced.take().expect("pending debounce value exists"));
                let target = provider.batch_size.unwrap_or(1);
                if batch.len() >= target {
                    self.commit_stream_callback(
                        &batch,
                        action,
                        provider.recover.as_deref(),
                        provider.batch_size,
                        depth,
                    )?;
                    batch.clear();
                }
            }

            ended.extend(page.ended_sources);
            if cursors.iter().all(|cursor| ended.contains(&cursor.source)) {
                if let Some(item) = debounced.take() {
                    batch.push(item);
                }
                if !batch.is_empty() {
                    self.commit_stream_callback(
                        &batch,
                        action,
                        provider.recover.as_deref(),
                        provider.batch_size,
                        depth,
                    )?;
                    batch.clear();
                }
                break;
            }
        }
        Ok(Value::Unit)
    }

    fn stream(
        &mut self,
        name: &str,
        values: Vec<Value>,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        match (name, values.as_slice()) {
            ("from_provider", [Value::String(source), Value::String(identity_label)]) => {
                self.provider_stream(source, identity_label)
            }
            ("from_list", [Value::List(items), Value::String(source_label)]) => {
                self.finite_stream(source_label.clone(), items.clone())
            }
            (
                "for_each",
                [
                    Value::Stream {
                        provider: Some(_), ..
                    },
                    action,
                ],
            ) => self.consume_provider_stream(&values[0], action, depth + 1),
            ("for_each", [Value::Stream { .. }, action]) => {
                let (items, _) = self.checked_stream_values(&values[0])?;
                self.items(items.len())?;
                for value in items {
                    self.step()?;
                    let result = self.invoke_predicate(action, value, depth + 1)?;
                    if !matches!(result, Value::Unit | Value::Null) {
                        return Err(error("ORNA-EVAL-TYPE"));
                    }
                    self.step()?;
                }
                Ok(Value::Unit)
            }
            ("batch", [Value::Stream { .. }, Value::Int(size)]) => {
                if let Value::Stream {
                    source_label,
                    source_digest,
                    provider: Some(provider),
                    ..
                } = &values[0]
                {
                    let size = self.positive_collection_size(size)?;
                    self.items(size)?;
                    if provider.batch_size.is_some() {
                        return Err(error("ORNA-EVAL-VALUE"));
                    }
                    let mut provider = provider.clone();
                    provider.batch_size = Some(size);
                    return self.provider_stream_value(
                        source_label.clone(),
                        *source_digest,
                        provider,
                    );
                }
                let (items, source_label) = self.checked_stream_values(&values[0])?;
                let size = self.positive_collection_size(size)?;
                let mut batches = Vec::new();
                for batch in items.chunks(size) {
                    self.step()?;
                    batches.push(Value::List(batch.to_vec()));
                    self.items(batches.len())?;
                }
                self.finite_stream(format!("{source_label}|batch:{size}"), batches)
            }
            ("buffer", [Value::Stream { .. }, Value::Int(capacity)]) => {
                if let Value::Stream {
                    source_label,
                    source_digest,
                    provider: Some(provider),
                    ..
                } = &values[0]
                {
                    let capacity = self.positive_collection_size(capacity)?;
                    self.items(capacity)?;
                    let mut provider = provider.clone();
                    provider.buffer_capacity = capacity;
                    return self.provider_stream_value(
                        source_label.clone(),
                        *source_digest,
                        provider,
                    );
                }
                let (items, source_label) = self.checked_stream_values(&values[0])?;
                self.positive_collection_size(capacity)?;
                // The immutable finite source is consumed under backpressure;
                // a bounded queue changes scheduling, never item order/content.
                self.finite_stream(format!("{source_label}|buffer"), items)
            }
            ("merge", [Value::List(streams)]) => {
                if streams.iter().any(|stream| {
                    matches!(
                        stream,
                        Value::Stream {
                            provider: Some(_),
                            ..
                        }
                    )
                }) {
                    self.items(streams.len())?;
                    if streams.is_empty() {
                        return self.provider_stream_value(
                            "merge:empty".to_owned(),
                            [0; 32],
                            ProviderStream {
                                sources: Vec::new(),
                                buffer_capacity: 1,
                                batch_size: None,
                                throttle_nanoseconds: None,
                                debounce_nanoseconds: None,
                                retry: None,
                                recover: None,
                            },
                        );
                    }
                    let mut sources = Vec::new();
                    let mut capacity = usize::MAX;
                    let mut merge_identity = Sha256::new();
                    merge_identity.update(b"orna.provider-merge.v1\0");
                    for stream in streams {
                        let Value::Stream {
                            provider: Some(provider),
                            source_digest,
                            ..
                        } = stream
                        else {
                            return Err(error("ORNA-EVAL-TYPE"));
                        };
                        if provider.batch_size.is_some()
                            || provider.throttle_nanoseconds.is_some()
                            || provider.debounce_nanoseconds.is_some()
                            || provider.retry.is_some()
                            || provider.recover.is_some()
                        {
                            return Err(error("ORNA-EVAL-VALUE"));
                        }
                        capacity = capacity.min(provider.buffer_capacity);
                        for source in &provider.sources {
                            self.step()?;
                            merge_identity.update((source.source.len() as u64).to_be_bytes());
                            merge_identity.update(source.source.as_bytes());
                            merge_identity.update(source.identity);
                            sources.push(source.clone());
                        }
                        merge_identity.update(source_digest);
                    }
                    let identity: [u8; 32] = merge_identity.finalize().into();
                    let label = format!(
                        "merge:{}",
                        identity
                            .iter()
                            .map(|byte| format!("{byte:02x}"))
                            .collect::<String>()
                    );
                    return self.provider_stream_value(
                        label,
                        identity,
                        ProviderStream {
                            sources,
                            buffer_capacity: capacity,
                            batch_size: None,
                            throttle_nanoseconds: None,
                            debounce_nanoseconds: None,
                            retry: None,
                            recover: None,
                        },
                    );
                }
                let mut sources = Vec::with_capacity(streams.len());
                for stream in streams {
                    let (items, source_label) = self.checked_stream_values(stream)?;
                    sources.push((source_label, items));
                }
                let total = sources.iter().try_fold(0usize, |total, (_, items)| {
                    total
                        .checked_add(items.len())
                        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))
                })?;
                self.items(total)?;
                let mut merged = Vec::with_capacity(total);
                // The reference leaves cross-source arrival to a live
                // scheduler. These finite materialized sources have no event
                // timestamps, so this evaluator uses a repeatable round-robin
                // admission schedule while preserving every source's order.
                for position in 0..sources
                    .iter()
                    .map(|(_, items)| items.len())
                    .max()
                    .unwrap_or(0)
                {
                    for (_, items) in &sources {
                        if let Some(value) = items.get(position) {
                            self.step()?;
                            merged.push(value.clone());
                        }
                    }
                }
                let mut merge_identity = Sha256::new();
                merge_identity.update(b"orna.merge.v1\0");
                merge_identity.update((sources.len() as u64).to_be_bytes());
                for (source_label, items) in &sources {
                    merge_identity.update((source_label.len() as u64).to_be_bytes());
                    merge_identity.update(source_label.as_bytes());
                    merge_identity.update(self.list_stream_digest(source_label, items)?);
                }
                let source_identity = merge_identity
                    .finalize()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                self.finite_stream(format!("merge:{source_identity}"), merged)
            }
            (
                "throttle",
                [
                    Value::Stream { .. },
                    Value::Duration {
                        seconds,
                        nanosecond,
                    },
                    Value::String(policy),
                ],
            ) => {
                if elapsed_total_nanoseconds(seconds, *nanosecond) <= BigInt::from(0u8) {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                if policy != "drop" {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                if let Value::Stream {
                    source_label,
                    source_digest,
                    provider: Some(provider),
                    ..
                } = &values[0]
                {
                    if provider.throttle_nanoseconds.is_some()
                        || provider.debounce_nanoseconds.is_some()
                    {
                        return Err(error("ORNA-EVAL-VALUE"));
                    }
                    let mut provider = provider.clone();
                    provider.throttle_nanoseconds =
                        Some(elapsed_total_nanoseconds(seconds, *nanosecond));
                    return self.provider_stream_value(
                        source_label.clone(),
                        *source_digest,
                        provider,
                    );
                }
                // The reference requires elapsed intervals but gives this
                // finite source no timestamp provider. Treat the materialized
                // list as one observation instant; explicit `drop` keeps its
                // first item and drops later same-instant values.
                let (items, source_label) = self.checked_stream_values(&values[0])?;
                let output = items.first().cloned().into_iter().collect();
                self.finite_stream(format!("{source_label}|throttle:drop"), output)
            }
            (
                "debounce",
                [
                    Value::Stream { .. },
                    Value::Duration {
                        seconds,
                        nanosecond,
                    },
                    Value::String(policy),
                ],
            ) => {
                if elapsed_total_nanoseconds(seconds, *nanosecond) <= BigInt::from(0u8) {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                if policy != "latest" {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                if let Value::Stream {
                    source_label,
                    source_digest,
                    provider: Some(provider),
                    ..
                } = &values[0]
                {
                    if provider.debounce_nanoseconds.is_some()
                        || provider.throttle_nanoseconds.is_some()
                    {
                        return Err(error("ORNA-EVAL-VALUE"));
                    }
                    let mut provider = provider.clone();
                    provider.debounce_nanoseconds =
                        Some(elapsed_total_nanoseconds(seconds, *nanosecond));
                    return self.provider_stream_value(
                        source_label.clone(),
                        *source_digest,
                        provider,
                    );
                }
                // The reference requires elapsed quiet intervals but gives
                // this finite source no timestamp provider. Treat its items as
                // one synchronous burst and apply explicit `latest` delivery.
                let (items, source_label) = self.checked_stream_values(&values[0])?;
                let output = items.last().cloned().into_iter().collect();
                self.finite_stream(format!("{source_label}|debounce:latest"), output)
            }
            ("retry", [Value::Stream { .. }, Value::Record(policy)]) => {
                if let Value::Stream {
                    source_label,
                    source_digest,
                    provider: Some(provider),
                    ..
                } = &values[0]
                {
                    let Some(Value::Int(max_attempts)) = policy.get("max_attempts") else {
                        return Err(error("ORNA-EVAL-TYPE"));
                    };
                    let Some(Value::Duration {
                        seconds: initial_seconds,
                        nanosecond: initial_nanos,
                    }) = policy.get("initial_delay")
                    else {
                        return Err(error("ORNA-EVAL-TYPE"));
                    };
                    let Some(Value::Duration {
                        seconds: maximum_seconds,
                        nanosecond: maximum_nanos,
                    }) = policy.get("max_delay")
                    else {
                        return Err(error("ORNA-EVAL-TYPE"));
                    };
                    let initial_delay_nanoseconds =
                        elapsed_total_nanoseconds(initial_seconds, *initial_nanos);
                    let max_delay_nanoseconds =
                        elapsed_total_nanoseconds(maximum_seconds, *maximum_nanos);
                    let max_attempts = max_attempts
                        .to_usize()
                        .filter(|attempts| {
                            *attempts > 0 && *attempts <= self.limits.max_collection_items
                        })
                        .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                    if initial_delay_nanoseconds < BigInt::from(0u8)
                        || max_delay_nanoseconds < initial_delay_nanoseconds
                        || provider.retry.is_some()
                    {
                        return Err(error("ORNA-EVAL-VALUE"));
                    }
                    let mut provider = provider.clone();
                    // `max_attempts` counts the first delivery attempt as
                    // well as retries; the initial delay applies before the
                    // first redelivery, then doubles up to `max_delay`.
                    provider.retry = Some(ProviderRetryPolicy {
                        max_attempts,
                        initial_delay_nanoseconds,
                        max_delay_nanoseconds,
                    });
                    return self.provider_stream_value(
                        source_label.clone(),
                        *source_digest,
                        provider,
                    );
                }
                let (items, source_label) = self.checked_stream_values(&values[0])?;
                let Some(Value::Int(max_attempts)) = policy.get("max_attempts") else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                let Some(Value::Duration {
                    seconds: initial_seconds,
                    nanosecond: initial_nanos,
                }) = policy.get("initial_delay")
                else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                let Some(Value::Duration {
                    seconds: maximum_seconds,
                    nanosecond: maximum_nanos,
                }) = policy.get("max_delay")
                else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                if !max_attempts.is_positive()
                    || elapsed_total_nanoseconds(initial_seconds, *initial_nanos)
                        < BigInt::from(0u8)
                    || elapsed_total_nanoseconds(maximum_seconds, *maximum_nanos)
                        < elapsed_total_nanoseconds(initial_seconds, *initial_nanos)
                {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                // ListStreamSource is replayable and cannot produce a blocked
                // provider delivery. A valid retry policy preserves it exactly.
                self.finite_stream(format!("{source_label}|retry"), items)
            }
            ("recover", [Value::Stream { .. }, handler]) => {
                if let Value::Stream {
                    source_label,
                    source_digest,
                    provider: Some(provider),
                    ..
                } = &values[0]
                {
                    if !matches!(handler, Value::Function { .. } | Value::Closure(_))
                        || provider.recover.is_some()
                    {
                        return Err(error("ORNA-EVAL-TYPE"));
                    }
                    let mut provider = provider.clone();
                    provider.recover = Some(Box::new(handler.clone()));
                    return self.provider_stream_value(
                        source_label.clone(),
                        *source_digest,
                        provider,
                    );
                }
                let (items, source_label) = self.checked_stream_values(&values[0])?;
                if !matches!(handler, Value::Function { .. } | Value::Closure(_)) {
                    return Err(error("ORNA-EVAL-TYPE"));
                }
                // The finite list source cannot fail decoding or polling, so
                // recovery has no failure to replace and retains every item.
                self.finite_stream(format!("{source_label}|recover"), items)
            }
            ("from_provider", [_, _])
            | ("from_list", [_, _])
            | ("for_each", [_, _])
            | ("batch", [_, _])
            | ("buffer", [_, _])
            | ("throttle", [_, _, _])
            | ("debounce", [_, _, _])
            | ("retry", [_, _])
            | ("recover", [_, _])
            | ("merge", [_]) => Err(error("ORNA-EVAL-TYPE")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn concurrent(
        &mut self,
        name: &str,
        values: Vec<Value>,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        match (name, values.as_slice()) {
            ("parallel", [Value::List(callbacks)]) => {
                self.items(callbacks.len())?;
                if callbacks.is_empty() {
                    return Ok(Value::List(Vec::new()));
                }
                self.depth(depth.saturating_add(1))?;
                let parent = self.cancellation.cloned();
                let mut child_effects = Vec::with_capacity(callbacks.len());
                if let Some(effects) = self.effects.as_deref_mut() {
                    for index in 0..callbacks.len() {
                        child_effects.push(effects.fork_task_child(index));
                    }
                } else {
                    child_effects.resize_with(callbacks.len(), || None);
                }
                let (results, _) = run_task_callbacks(
                    callbacks,
                    child_effects,
                    depth.saturating_add(1),
                    parent.clone(),
                    self.functions,
                    self.aliases,
                    self.session_functions,
                    self.limits,
                    self.repl_bindings,
                    self.restrict_function_names,
                    self.reject_unhandled_field_calls,
                    true,
                    false,
                );
                if parent.as_ref().is_some_and(CancellationToken::is_requested) {
                    return Err(error("ORNA-EVAL-CANCELLED"));
                }
                let failures = results
                    .iter()
                    .filter_map(|result| result.as_ref()?.as_ref().err())
                    .filter(|failure| failure.code() != "ORNA-EVAL-CANCELLED")
                    .cloned()
                    .collect::<Vec<_>>();
                if !failures.is_empty() {
                    return Err(aggregate_task_failures(&failures));
                }
                let mut output = Vec::with_capacity(results.len());
                for result in results {
                    match result {
                        Some(Ok(value)) => output.push(value),
                        Some(Err(failure)) => return Err(failure),
                        None => return Err(error("ORNA-EVAL-CANCELLED")),
                    }
                }
                Ok(Value::List(output))
            }
            ("race", [Value::List(callbacks)]) => {
                self.items(callbacks.len())?;
                if callbacks.is_empty() {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                self.depth(depth.saturating_add(1))?;
                let parent = self.cancellation.cloned();
                let mut child_effects = Vec::with_capacity(callbacks.len());
                if let Some(effects) = self.effects.as_deref_mut() {
                    for index in 0..callbacks.len() {
                        child_effects.push(effects.fork_task_child(index));
                    }
                } else {
                    child_effects.resize_with(callbacks.len(), || None);
                }
                let (results, winner) = run_task_callbacks(
                    callbacks,
                    child_effects,
                    depth.saturating_add(1),
                    parent.clone(),
                    self.functions,
                    self.aliases,
                    self.session_functions,
                    self.limits,
                    self.repl_bindings,
                    self.restrict_function_names,
                    self.reject_unhandled_field_calls,
                    false,
                    true,
                );
                if parent.as_ref().is_some_and(CancellationToken::is_requested) {
                    return Err(error("ORNA-EVAL-CANCELLED"));
                }
                if let Some(winner) = winner {
                    return Ok(winner);
                }
                results
                    .into_iter()
                    .enumerate()
                    .find_map(|(_, result)| {
                        result.and_then(|result| match result {
                            Err(failure) if failure.code() != "ORNA-EVAL-CANCELLED" => {
                                Some(Err(failure))
                            }
                            _ => None,
                        })
                    })
                    .unwrap_or_else(|| Err(error("ORNA-EVAL-ERROR")))
            }
            (
                "timeout",
                [
                    callback,
                    Value::Duration {
                        seconds,
                        nanosecond,
                    },
                ],
            ) => {
                self.depth(depth.saturating_add(1))?;
                if seconds.is_negative() {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                let seconds = seconds.to_u64().ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                let timeout = std::time::Duration::new(seconds, *nanosecond);
                let shared_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let parent = self.cancellation.cloned();
                let functions = self.functions;
                let aliases = self.aliases;
                let session_functions = self.session_functions;
                let limits = self.limits;
                let repl_bindings = self.repl_bindings;
                let restrict_function_names = self.restrict_function_names;
                let reject_unhandled_field_calls = self.reject_unhandled_field_calls;
                let child_effects = self
                    .effects
                    .as_deref_mut()
                    .and_then(|effects| effects.fork_task_child(0));
                let result = std::thread::scope(|scope| {
                    let (sender, receiver) = std::sync::mpsc::channel();
                    let shared_for_task = Arc::clone(&shared_cancel);
                    let callback = callback.clone();
                    let task_parent = parent.clone();
                    scope.spawn(move || {
                        let cancellation =
                            CancellationToken::child(Arc::clone(&shared_for_task), task_parent);
                        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            invoke_task_callback(
                                callback,
                                functions,
                                aliases,
                                session_functions,
                                limits,
                                cancellation,
                                repl_bindings,
                                restrict_function_names,
                                reject_unhandled_field_calls,
                                depth.saturating_add(1),
                                child_effects,
                            )
                        }))
                        .unwrap_or_else(|_| Err(error("ORNA-EVAL-ERROR")));
                        let _ = sender.send(result);
                    });
                    let started = std::time::Instant::now();
                    loop {
                        if parent.as_ref().is_some_and(CancellationToken::is_requested) {
                            shared_cancel.store(true, std::sync::atomic::Ordering::Release);
                            while receiver.recv().is_ok() {}
                            return Err(error("ORNA-EVAL-CANCELLED"));
                        }
                        let remaining = timeout.saturating_sub(started.elapsed());
                        if remaining.is_zero() {
                            match receiver.try_recv() {
                                Ok(result) => return result,
                                Err(std::sync::mpsc::TryRecvError::Empty) => {
                                    shared_cancel.store(true, std::sync::atomic::Ordering::Release);
                                    let _ = receiver.recv();
                                    return Err(error("ORNA-EVAL-TIMEOUT"));
                                }
                                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                    return Err(error("ORNA-EVAL-ERROR"));
                                }
                            }
                        }
                        match receiver
                            .recv_timeout(remaining.min(std::time::Duration::from_millis(10)))
                        {
                            Ok(result) => return result,
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                                return Err(error("ORNA-EVAL-ERROR"));
                            }
                        }
                    }
                })?;
                Ok(result)
            }
            ("parallel" | "race", [_]) | ("timeout", [_, _]) => Err(error("ORNA-EVAL-TYPE")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn time(&mut self, name: &str, values: Vec<Value>) -> Result<Value, EvaluationError> {
        match (name, values.as_slice()) {
            (
                "offset_at",
                [
                    Value::Instant {
                        unix_seconds,
                        nanosecond,
                    },
                    Value::String(zone),
                ],
            ) => {
                self.step()?;
                let zone = resolve_time_zone(zone).map_err(|_| error("ORNA-EVAL-VALUE"))?;
                let instant = Instant::new(*unix_seconds, *nanosecond)
                    .map_err(|_| error("ORNA-EVAL-VALUE"))?;
                let offset = zone
                    .at(instant)
                    .map_err(|_| error("ORNA-EVAL-VALUE"))?
                    .offset_seconds;
                Ok(Value::Int(BigInt::from(offset)))
            }
            (
                "resolve_local",
                [
                    Value::String(local),
                    Value::String(zone),
                    Value::String(ambiguous),
                ],
            ) => self.resolve_local_time(local, zone, ambiguous, None),
            (
                "resolve_local",
                [
                    Value::String(local),
                    Value::String(zone),
                    Value::String(ambiguous),
                    gap,
                ],
            ) => {
                let gap = match gap {
                    Value::Null | Value::Option(None) => None,
                    Value::String(gap) => Some(gap.as_str()),
                    Value::Option(Some(inner)) => match inner.as_ref() {
                        Value::String(gap) => Some(gap.as_str()),
                        _ => return Err(error("ORNA-EVAL-TYPE")),
                    },
                    _ => return Err(error("ORNA-EVAL-TYPE")),
                };
                self.resolve_local_time(local, zone, ambiguous, gap)
            }
            (
                "duration.compact.format"
                | "duration.clock.format"
                | "duration.words.format"
                | "duration.iso.format",
                [
                    Value::Duration {
                        seconds,
                        nanosecond,
                    },
                ],
            ) => {
                self.step()?;
                format_duration_context(None)?;
                self.string(format_duration(name, seconds, *nanosecond))
                    .map(Value::String)
            }
            (
                "duration.compact.format"
                | "duration.clock.format"
                | "duration.words.format"
                | "duration.iso.format",
                [
                    Value::Duration {
                        seconds,
                        nanosecond,
                    },
                    context,
                ],
            ) => {
                self.step()?;
                format_duration_context(Some(context))?;
                self.string(format_duration(name, seconds, *nanosecond))
                    .map(Value::String)
            }
            ("offset_at" | "resolve_local", _) => Err(error("ORNA-EVAL-TYPE")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn resolve_local_time(
        &mut self,
        local_text: &str,
        zone_name: &str,
        ambiguous: &str,
        gap_adjustment: Option<&str>,
    ) -> Result<Value, EvaluationError> {
        if !matches!(ambiguous, "reject" | "earlier" | "later")
            || !matches!(
                gap_adjustment,
                None | Some("reject" | "shift_forward" | "shift_backward")
            )
        {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        self.step()?;
        let local = parse_local_datetime(local_text).ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
        let zone = resolve_time_zone(zone_name).map_err(|_| error("ORNA-EVAL-VALUE"))?;
        let instant = match zone
            .resolve_local(local)
            .map_err(|_| error("ORNA-EVAL-VALUE"))?
        {
            LocalTimeResolution::Unique { instant, .. } => instant,
            LocalTimeResolution::Ambiguous { earlier, .. } if ambiguous == "earlier" => earlier,
            LocalTimeResolution::Ambiguous { later, .. } if ambiguous == "later" => later,
            LocalTimeResolution::Ambiguous { .. } => return Err(error("ORNA-EVAL-VALUE")),
            LocalTimeResolution::Nonexistent { before, after } => {
                let adjustment = match gap_adjustment {
                    Some("shift_forward") => "shift_forward",
                    Some("shift_backward") => "shift_backward",
                    None | Some("reject") => return Err(error("ORNA-EVAL-VALUE")),
                    _ => return Err(error("ORNA-EVAL-VALUE")),
                };
                let offset_before = zone
                    .at(before)
                    .map_err(|_| error("ORNA-EVAL-VALUE"))?
                    .offset_seconds;
                let offset_after = zone
                    .at(after)
                    .map_err(|_| error("ORNA-EVAL-VALUE"))?
                    .offset_seconds;
                let utc = resolve_time_zone("UTC").map_err(|_| error("ORNA-EVAL-VALUE"))?;
                let LocalTimeResolution::Unique {
                    instant: local_as_utc,
                    ..
                } = utc
                    .resolve_local(local)
                    .map_err(|_| error("ORNA-EVAL-VALUE"))?
                else {
                    return Err(error("ORNA-EVAL-VALUE"));
                };
                let offset = if adjustment == "shift_forward" {
                    offset_before
                } else {
                    offset_after
                };
                let shifted = Instant::new(
                    local_as_utc
                        .unix_seconds
                        .checked_sub(i64::from(offset))
                        .ok_or_else(|| error("ORNA-EVAL-VALUE"))?,
                    local.nanosecond,
                )
                .map_err(|_| error("ORNA-EVAL-VALUE"))?;
                // Validate the shifted instant against the selected zone's
                // supported range and transition table before returning it.
                zone.at(shifted).map_err(|_| error("ORNA-EVAL-VALUE"))?;
                shifted
            }
        };
        Ok(Value::Instant {
            unix_seconds: instant.unix_seconds,
            nanosecond: instant.nanosecond,
        })
    }
    fn base64(&mut self, name: &str, values: Vec<Value>) -> Result<Value, EvaluationError> {
        match (name, values.as_slice()) {
            ("__encode" | "encode", [Value::Blob(bytes)]) => {
                self.items(bytes.len())?;
                self.string(encode_base64(bytes)).map(Value::String)
            }
            ("__decode" | "decode", [Value::String(text)]) => {
                self.string(text.clone())?;
                let bytes = decode_base64(text)?;
                self.items(bytes.len())?;
                Ok(Value::Blob(bytes))
            }
            ("__encode" | "encode" | "__decode" | "decode", _) => Err(error("ORNA-EVAL-TYPE")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn random(
        &mut self,
        name: &str,
        values: Vec<Value>,
        callee: &Expr,
    ) -> Result<Value, EvaluationError> {
        self.step()?;
        match (name, values.as_slice()) {
            ("bytes" | "__bytes", [Value::Int(count)]) => {
                let count = count.to_usize().ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                self.items(count)?;
                self.random_entropy(callee, count).map(Value::Blob)
            }
            ("integer" | "__integer", [Value::Int(lower), Value::Int(upper)]) => {
                if upper <= lower {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                let width = upper - lower;
                let width = width.to_biguint().ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                let bit_count = width.bits();
                let byte_count =
                    usize::try_from(bit_count.div_ceil(8)).map_err(|_| error("ORNA-EVAL-LIMIT"))?;
                self.items(byte_count)?;
                let high_byte_bits = (bit_count % 8) as u8;
                loop {
                    self.step()?;
                    let mut sample = self.random_entropy(callee, byte_count)?;
                    if high_byte_bits != 0 {
                        let mask = (1u8 << high_byte_bits) - 1;
                        *sample.last_mut().ok_or_else(|| error("ORNA-EVAL-VALUE"))? &= mask;
                    }
                    let candidate = BigInt::from_bytes_le(Sign::Plus, &sample);
                    if candidate < BigInt::from(width.clone()) {
                        return Ok(Value::Int(lower + candidate));
                    }
                }
            }
            ("choose" | "__choose", [Value::List(values)]) => {
                if values.is_empty() {
                    return Ok(Value::Option(None));
                }
                self.items(values.len())?;
                let index = match self.random(
                    "integer",
                    vec![Value::Int(0.into()), Value::Int(BigInt::from(values.len()))],
                    callee,
                )? {
                    Value::Int(index) => {
                        index.to_usize().ok_or_else(|| error("ORNA-EVAL-VALUE"))?
                    }
                    _ => return Err(error("ORNA-EVAL-VALUE")),
                };
                let selected = values
                    .get(index)
                    .cloned()
                    .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                Ok(Value::Option(Some(Box::new(selected))))
            }
            ("shuffle" | "__shuffle", [Value::List(values)]) => {
                self.items(values.len())?;
                let mut shuffled = values.clone();
                for index in (1..shuffled.len()).rev() {
                    self.step()?;
                    let selected = match self.random(
                        "integer",
                        vec![Value::Int(0.into()), Value::Int(BigInt::from(index + 1))],
                        callee,
                    )? {
                        Value::Int(selected) => selected
                            .to_usize()
                            .ok_or_else(|| error("ORNA-EVAL-VALUE"))?,
                        _ => return Err(error("ORNA-EVAL-VALUE")),
                    };
                    shuffled.swap(index, selected);
                }
                Ok(Value::List(shuffled))
            }
            (
                "bytes" | "__bytes" | "integer" | "__integer" | "choose" | "__choose" | "shuffle"
                | "__shuffle",
                _,
            ) => Err(error("ORNA-EVAL-TYPE")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }

    fn random_entropy(&mut self, callee: &Expr, count: usize) -> Result<Vec<u8>, EvaluationError> {
        let expected_count = count;
        let count = CanonicalValue::new(Raw::Int(BigInt::from(count)))
            .map_err(|_| error("ORNA-EVAL-VALUE"))?;
        let arguments = [count];
        let remaining = self.limits.max_steps.saturating_sub(self.steps);
        let mut budget = StepBudget::new(remaining);
        let cancellation = self.cancellation;
        let result = self
            .effects
            .as_deref_mut()
            .ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?
            .handle_registered_with_cancellation_and_budget(
                "std.random.entropy",
                callee,
                &arguments,
                &mut budget,
                cancellation,
            );
        let debited = remaining - budget.remaining();
        self.steps = self
            .steps
            .checked_add(debited)
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        let value = result?.ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?;
        let Raw::Bytes(bytes) = value.raw() else {
            return Err(error("ORNA-EVAL-TYPE"));
        };
        if bytes.len() != expected_count {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        self.items(bytes.len())?;
        Ok(bytes.clone())
    }

    fn hash(&mut self, name: &str, values: Vec<Value>) -> Result<Value, EvaluationError> {
        self.step()?;
        match (name, values.as_slice()) {
            ("sha256", [Value::Blob(input)]) => {
                if input.len() > self.limits.max_string_bytes {
                    return Err(error("ORNA-EVAL-LIMIT"));
                }
                Ok(Value::Blob(Sha256::digest(input).to_vec()))
            }
            ("sha256_text", [Value::String(input)]) => {
                Ok(Value::Blob(Sha256::digest(input.as_bytes()).to_vec()))
            }
            ("domain_sha256", [Value::String(domain), Value::Blob(payload)]) => {
                if domain.is_empty()
                    || !domain.is_ascii()
                    || domain.as_bytes().contains(&0)
                    || payload.len() > self.limits.max_string_bytes
                {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                let mut digest = Sha256::new();
                digest.update(domain.as_bytes());
                digest.update([0]);
                digest.update(payload);
                Ok(Value::Blob(digest.finalize().to_vec()))
            }
            ("to_hex", [Value::Blob(digest)]) if digest.len() == 32 => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                let mut output = String::with_capacity(64);
                for byte in digest {
                    output.push(char::from(HEX[usize::from(byte >> 4)]));
                    output.push(char::from(HEX[usize::from(byte & 0x0f)]));
                }
                self.string(output).map(Value::String)
            }
            ("from_hex", [Value::String(encoded)]) => {
                let decoded = if encoded.len() == 64
                    && encoded
                        .as_bytes()
                        .iter()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
                {
                    let mut bytes = Vec::with_capacity(32);
                    for pair in encoded.as_bytes().chunks_exact(2) {
                        let high = (pair[0] as char).to_digit(16).expect("validated hex digit");
                        let low = (pair[1] as char).to_digit(16).expect("validated hex digit");
                        bytes.push(((high << 4) | low) as u8);
                    }
                    Some(Box::new(Value::Blob(bytes)))
                } else {
                    None
                };
                Ok(Value::Option(decoded))
            }
            ("sha256" | "sha256_text" | "domain_sha256" | "to_hex" | "from_hex", _) => {
                Err(error("ORNA-EVAL-TYPE"))
            }
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn ovb_codec(&mut self, name: &str, values: Vec<Value>) -> Result<Value, EvaluationError> {
        match (name, values.as_slice()) {
            ("__encode", [value]) => {
                let bytes = value
                    .clone()
                    .canonical()?
                    .encode()
                    .map_err(|_| error("ORNA-EVAL-VALUE"))?;
                self.items(bytes.len())?;
                Ok(Value::Blob(bytes))
            }
            ("__decode", [Value::Blob(bytes)]) => {
                self.items(bytes.len())?;
                let value = CanonicalValue::decode(bytes).map_err(|_| error("ORNA-EVAL-VALUE"))?;
                Value::from_canonical(&value, self, 0)
            }
            ("__encode" | "__decode", _) => Err(error("ORNA-EVAL-TYPE")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn json_codec(&mut self, name: &str, values: Vec<Value>) -> Result<Value, EvaluationError> {
        match (name, values.as_slice()) {
            ("__encode", [value]) => {
                let mut output = String::new();
                write_json_value(value, &mut output, self, 0)?;
                self.string(output).map(Value::String)
            }
            ("__decode", [Value::String(input)])
            | ("__decode_with_options", [Value::String(input), Value::Bool(_)]) => {
                self.string(input.clone())?;
                let node = parse_json_node(input)?;
                json_node_to_value(node, self, 0)
            }
            ("__decode" | "__decode_with_options", _) => Err(error("ORNA-EVAL-TYPE")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn orna_codec(&mut self, name: &str, values: Vec<Value>) -> Result<Value, EvaluationError> {
        match (name, values.as_slice()) {
            ("__encode", [value]) => {
                let mut output = encode_orna_value(value, 0)?;
                output.push('\n');
                self.string(output).map(Value::String)
            }
            ("__decode", [Value::String(input)]) => {
                self.string(input.clone())?;
                let parsed = parse_expression(input);
                if !parsed.is_ok() || !is_orna_data_expression(&parsed.value) {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                let mut scope = Scope(
                    BTreeMap::new(),
                    BTreeSet::new(),
                    BTreeSet::new(),
                    NominalDefinitions::new(),
                    BTreeSet::new(),
                );
                self.evaluate(&parsed.value, &mut scope, 0)
            }
            ("__decode", _) => Err(error("ORNA-EVAL-TYPE")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn money(&mut self, name: &str, values: Vec<Value>) -> Result<Value, EvaluationError> {
        self.step()?;
        match (name, values.as_slice()) {
            ("__quantize", [amount, Value::Int(scale), Value::String(rounding)]) => {
                let scale = scale
                    .to_usize()
                    .filter(|scale| *scale <= DEFAULT_INTEGER_DIGITS)
                    .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                let decimal = money_amount(amount)?;
                let rounded = quantize_decimal(&decimal, scale, rounding)?;
                replace_money_amount(amount, rounded)
            }
            (
                "__allocate",
                [
                    amount,
                    Value::List(weight_values),
                    Value::Int(scale),
                    rounding,
                ],
            ) => {
                self.items(weight_values.len())?;
                let scale = scale
                    .to_usize()
                    .filter(|scale| *scale <= DEFAULT_INTEGER_DIGITS)
                    .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                let rounding = match rounding {
                    Value::Null | Value::Option(None) => None,
                    Value::String(mode) => Some(mode.as_str()),
                    Value::Option(Some(inner)) => match inner.as_ref() {
                        Value::String(mode) => Some(mode.as_str()),
                        _ => return Err(error("ORNA-EVAL-TYPE")),
                    },
                    _ => return Err(error("ORNA-EVAL-TYPE")),
                };
                let weights = weight_values
                    .iter()
                    .map(|weight| match weight {
                        Value::Int(weight) if !weight.is_negative() => self.integer(weight.clone()),
                        Value::Int(_) => Err(error("ORNA-EVAL-VALUE")),
                        _ => Err(error("ORNA-EVAL-TYPE")),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let shares = allocate_minor_units(amount, &weights, scale, rounding)?;
                self.items(shares.len())?;
                shares
                    .into_iter()
                    .map(|share| replace_money_amount(amount, share))
                    .collect::<Result<Vec<_>, _>>()
                    .map(Value::List)
            }
            (
                "__format",
                [
                    amount,
                    Value::String(currency_code),
                    Value::Int(minor_digits),
                    Value::String(rounding),
                    Value::String(locale),
                ],
            ) => {
                let minor_digits = minor_digits
                    .to_usize()
                    .filter(|digits| *digits <= DEFAULT_INTEGER_DIGITS)
                    .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                let formatted =
                    format_money_value(amount, currency_code, minor_digits, rounding, locale)?;
                self.string(formatted).map(Value::String)
            }
            ("__quantize" | "__allocate" | "__format", _) => Err(error("ORNA-EVAL-TYPE")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn list_stream_digest(
        &mut self,
        source_label: &str,
        values: &[Value],
    ) -> Result<[u8; 32], EvaluationError> {
        self.string(source_label.to_owned())?;
        self.items(values.len())?;
        let mut encoded_values = Vec::with_capacity(values.len());
        for value in values {
            self.step()?;
            encoded_values.push(value.clone().raw()?);
            self.items(encoded_values.len())?;
        }
        let identity = Raw::Array(vec![
            Raw::Text(source_label.to_owned()),
            Raw::Array(encoded_values),
        ]);
        domain_digest("orna.list.v1", &identity).map_err(|_| error("ORNA-EVAL-VALUE"))
    }

    fn collection(
        &mut self,
        name: &str,
        values: Vec<Value>,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        match (name, values.as_slice()) {
            ("recursive_cte" | "__recursive_cte", [Value::List(anchor), recursive_term]) => {
                self.recursive_cte(anchor, recursive_term, depth)
            }
            ("__list_length", [Value::List(values)]) => self.count(values),
            ("__list_concat", [Value::List(left), Value::List(right)]) => self.union(left, right),
            ("__numeric_sum", [Value::List(values)]) => self.sum(values),
            ("__stable_sort", [Value::List(values), key]) => self.sort_by(values, key, depth),
            ("__minimum", [Value::List(values)]) => self.extreme("min", values),
            ("__maximum", [Value::List(values)]) => self.extreme("max", values),
            ("__group_by", [Value::List(values), key]) => self.group_by(values, key, depth),
            ("__rank", [Value::List(values), key]) => self.rank(values, key, depth),
            ("__asof_join", [Value::List(left), Value::List(right), time, by]) => {
                self.asof_join(left, right, time, by, depth)
            }
            ("__bucket_by", [Value::List(rows), period, zone]) => {
                let spec = bucket_by_spec(period, Some(zone))?;
                let mut state = RelationBucketState::try_new(spec).map_err(bucket_error)?;
                self.items(rows.len())?;
                let mut groups = Vec::new();
                for row in rows {
                    self.step()?;
                    if let Some(bucket) = state.push(row.clone()).map_err(bucket_error)? {
                        self.items(bucket.values.len())?;
                        groups.push(Value::List(bucket.values));
                        self.items(groups.len())?;
                    }
                }
                if let Some(bucket) = state.finish() {
                    self.items(bucket.values.len())?;
                    groups.push(Value::List(bucket.values));
                    self.items(groups.len())?;
                }
                Ok(Value::List(groups))
            }
            ("chunk", [Value::List(values), Value::Int(size)]) => {
                self.items(values.len())?;
                let size = self.positive_collection_size(size)?;
                for _ in values {
                    self.step()?;
                }
                let mut chunks = Vec::new();
                for chunk in values.chunks(size) {
                    self.items(chunk.len())?;
                    self.step()?;
                    chunks.push(Value::List(chunk.to_vec()));
                    self.items(chunks.len())?;
                }
                Ok(Value::List(chunks))
            }
            ("flatten", [Value::List(values)]) => {
                self.items(values.len())?;
                let mut flattened = Vec::new();
                for value in values {
                    self.step()?;
                    let Value::List(inner) = value else {
                        return Err(error("ORNA-EVAL-TYPE"));
                    };
                    self.items(inner.len())?;
                    for value in inner {
                        self.step()?;
                        flattened.push(value.clone());
                        self.items(flattened.len())?;
                    }
                }
                Ok(Value::List(flattened))
            }
            ("distinct", [Value::List(values)]) | ("unique", [Value::List(values)]) => {
                self.distinct(values)
            }
            ("union", [Value::List(left), Value::List(right)]) => self.union(left, right),
            ("count", [Value::List(values)]) => self.count(values),
            ("first", [Value::List(values)]) => self.first(values),
            ("last", [Value::List(values)]) => self.last(values),
            ("min" | "max", [Value::List(values)]) => self.extreme(name, values),
            ("sum", [Value::List(values)]) => self.sum(values),
            ("one", [Value::List(values)]) => self.one(values, None, depth),
            ("one", [Value::List(values), predicate]) => self.one(values, Some(predicate), depth),
            ("every", [Value::List(values), predicate]) => self.every(values, predicate, depth),
            ("exists", [Value::List(values), predicate]) => self.exists(values, predicate, depth),
            ("take", [Value::List(values), Value::Int(count)]) => self.take(values, count),
            ("drop", [Value::List(values), Value::Int(count)]) => self.drop(values, count),
            ("map", [Value::List(values), transform]) => self.map(values, transform, depth),
            ("flat_map", [Value::List(values), transform]) => {
                self.flat_map(values, transform, depth)
            }
            ("sort_by", [Value::List(values), key]) => self.sort_by(values, key, depth),
            ("rank", [Value::List(values), key]) => self.rank(values, key, depth),
            ("filter", [Value::List(values), predicate]) => self.filter(values, predicate, depth),
            ("partition", [Value::List(values), predicate]) => {
                self.partition(values, predicate, depth)
            }
            ("split_when", [Value::List(values), predicate]) => {
                self.split_when(values, predicate, depth)
            }
            ("group_by", [Value::List(values), key]) => self.group_by(values, key, depth),
            ("asof_join", [Value::List(left), Value::List(right), time, by]) => {
                self.asof_join(left, right, time, by, depth)
            }
            ("zip", [Value::List(left), Value::List(right)]) => self.zipped(left, right, false),
            ("zip_exact", [Value::List(left), Value::List(right)]) => {
                self.zipped(left, right, true)
            }
            ("pairs", [Value::List(values)]) => {
                self.items(values.len())?;
                for _ in values {
                    self.step()?;
                }
                let mut pairs = Vec::new();
                for pair in values.windows(2) {
                    self.step()?;
                    self.items(pair.len())?;
                    pairs.push(Value::Tuple(pair.to_vec()));
                    self.items(pairs.len())?;
                }
                Ok(Value::List(pairs))
            }
            ("window", [Value::List(values), Value::Int(size)]) => {
                self.windows(values, size, &BigInt::from(1))
            }
            ("window", [Value::List(values), Value::Int(size), Value::Int(step)]) => {
                self.windows(values, size, step)
            }
            ("__strictly_ordered_time_series", [Value::List(points)]) => {
                self.strictly_ordered_time_series(points)
            }
            ("__window_rate", [Value::List(points), Value::Int(size), Value::Int(step)]) => {
                self.window_time_series_statistics(points, size, step, "rate")
            }
            (
                "__window_rate_integrate",
                [Value::List(points), Value::Int(size), Value::Int(step)],
            ) => self.window_time_series_statistics(points, size, step, "rate_integrate"),
            ("__window_derivative", [Value::List(points), Value::Int(size), Value::Int(step)]) => {
                self.window_time_series_statistics(points, size, step, "derivative")
            }
            ("__window_integrate", [Value::List(points), Value::Int(size), Value::Int(step)]) => {
                self.window_time_series_statistics(points, size, step, "integrate")
            }
            ("chunk", [_, _])
            | (
                "flatten" | "distinct" | "unique" | "pairs" | "count" | "first" | "last" | "min"
                | "max" | "sum",
                [_],
            )
            | ("one", [_] | [_, _])
            | ("every" | "exists", [_, _])
            | ("count", [_, _])
            | ("union", [_, _])
            | ("take", [_, _])
            | ("drop", [_, _])
            | ("map", [_, _])
            | ("flat_map", [_, _])
            | ("sort_by" | "rank", [_, _])
            | ("filter", [_, _])
            | ("partition", [_, _])
            | ("split_when", [_, _])
            | ("group_by", [_, _])
            | ("asof_join", [_, _, _, _])
            | ("__bucket_by", [_, _, _])
            | ("zip" | "zip_exact", [_, _])
            | ("window", [_, _] | [_, _, _]) => Err(error("ORNA-EVAL-TYPE")),
            ("__strictly_ordered_time_series", [_]) => Err(error("ORNA-EVAL-TYPE")),
            (
                "__window_rate"
                | "__window_rate_integrate"
                | "__window_derivative"
                | "__window_integrate",
                [_, _, _],
            ) => Err(error("ORNA-EVAL-TYPE")),
            ("__list_length" | "__numeric_sum", [_])
            | ("__list_concat", [_, _])
            | ("__stable_sort" | "__group_by" | "__rank", [_, _])
            | ("__minimum" | "__maximum", [_])
            | ("__asof_join", [_, _, _, _]) => Err(error("ORNA-EVAL-TYPE")),
            ("recursive_cte" | "__recursive_cte", [_, _]) => Err(error("ORNA-EVAL-TYPE")),
            ("asof_join", _) => Err(error("ORNA-EVAL-ARGUMENT")),
            ("recursive_cte" | "__recursive_cte", _) => Err(error("ORNA-EVAL-ARGUMENT")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }

    /// Computes a finite breadth-first recursive query, using one canonical
    /// identity set for the anchor and every recursive round. The invocation
    /// owns that fold; input anchor order (including any prior lateral
    /// flat-map order) determines first values and the breadth-first output
    /// order. The returned order is preserved by downstream window frames;
    /// sibling calls keep separate folds even when those frames overlap in
    /// value identity.
    fn recursive_cte(
        &mut self,
        anchor: &[Value],
        recursive_term: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        let mut fold = DistinctValueFold::default();
        let mut frontier = Vec::new();
        self.items(anchor.len())?;
        for value in anchor {
            self.step()?;
            if fold.insert(value.clone())? {
                frontier.push(value.clone());
                self.items(fold.len())?;
            }
        }

        while !frontier.is_empty() {
            let mut next_frontier = Vec::new();
            for row in std::mem::take(&mut frontier) {
                self.step()?;
                let next = self.invoke_predicate(recursive_term, row, depth + 1)?;
                let Value::List(candidates) = next else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                self.items(candidates.len())?;
                for value in candidates {
                    self.step()?;
                    if fold.insert(value.clone())? {
                        next_frontier.push(value);
                        self.items(fold.len())?;
                        self.items(next_frontier.len())?;
                    }
                }
            }
            frontier = next_frontier;
        }

        Ok(Value::List(fold.into_values()))
    }

    fn map(
        &mut self,
        values: &[Value],
        transform: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut mapped = Vec::with_capacity(values.len());
        for value in values {
            mapped.push(self.invoke_predicate(transform, value.clone(), depth + 1)?);
            self.items(mapped.len())?;
        }
        Ok(Value::List(mapped))
    }
    fn flat_map(
        &mut self,
        values: &[Value],
        transform: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut flattened = Vec::new();
        for value in values {
            let Value::List(inner) = self.invoke_predicate(transform, value.clone(), depth + 1)?
            else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            self.items(inner.len())?;
            for value in inner {
                flattened.push(value);
                self.items(flattened.len())?;
            }
        }
        Ok(Value::List(flattened))
    }
    fn sort_by(
        &mut self,
        values: &[Value],
        key: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut keyed = Vec::with_capacity(values.len());
        for value in values {
            // Evaluate every callback in source order before considering any
            // reordering. This preserves effect/error observability even when
            // the eventual sort can be optimized by its caller.
            let sort_key = self.invoke_predicate(key, value.clone(), depth + 1)?;
            lawful_sort_key(&sort_key)?;
            keyed.push((sort_key, value.clone()));
            self.items(keyed.len())?;
        }

        // A fallible insertion sort keeps comparison errors explicit while
        // retaining source order for equal keys. It is bounded by the already
        // admitted finite-list item limit.
        for index in 1..keyed.len() {
            let mut current = index;
            while current > 0
                && compare_sort_keys(&keyed[current - 1].0, &keyed[current].0)?
                    == std::cmp::Ordering::Greater
            {
                keyed.swap(current - 1, current);
                current -= 1;
            }
        }
        Ok(Value::List(
            keyed.into_iter().map(|(_, value)| value).collect(),
        ))
    }
    fn rank(
        &mut self,
        values: &[Value],
        key: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut keyed = Vec::with_capacity(values.len());
        for value in values {
            // Callback evaluation is observable and must consume one bounded
            // step even when the callback body is only an identity lookup.
            self.step()?;
            let sort_key = self.invoke_predicate(key, value.clone(), depth + 1)?;
            lawful_sort_key(&sort_key)?;
            keyed.push((sort_key, value.clone()));
            self.items(keyed.len())?;
        }

        // Stable insertion sort preserves source order for equal keys while
        // keeping each comparison fallible and explicitly budgeted.
        for index in 1..keyed.len() {
            let mut current = index;
            while current > 0 {
                self.step()?;
                if compare_sort_keys(&keyed[current - 1].0, &keyed[current].0)?
                    != std::cmp::Ordering::Greater
                {
                    break;
                }
                keyed.swap(current - 1, current);
                current -= 1;
            }
        }

        let mut ranked = Vec::with_capacity(keyed.len());
        let mut competition_rank = 1usize;
        let mut previous_key = None;
        for (index, (sort_key, value)) in keyed.into_iter().enumerate() {
            if index > 0 {
                self.step()?;
                if compare_sort_keys(
                    previous_key
                        .as_ref()
                        .ok_or_else(|| error("ORNA-EVAL-VALUE"))?,
                    &sort_key,
                )? != std::cmp::Ordering::Equal
                {
                    competition_rank = index
                        .checked_add(1)
                        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
                }
            }
            let rank = self.integer(BigInt::from(competition_rank))?;
            previous_key = Some(sort_key);
            ranked.push(Value::Tuple(vec![value, Value::Int(rank)]));
            self.items(ranked.len())?;
        }
        Ok(Value::List(ranked))
    }
    fn distinct(&mut self, values: &[Value]) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut fold = DistinctValueFold::default();
        for value in values {
            self.step()?;
            if !fold.insert(value.clone())? {
                continue;
            }
            self.step()?;
            self.items(fold.len())?;
        }
        Ok(Value::List(fold.into_values()))
    }
    fn union(&mut self, left: &[Value], right: &[Value]) -> Result<Value, EvaluationError> {
        self.items(left.len())?;
        self.items(right.len())?;
        let length = left
            .len()
            .checked_add(right.len())
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        self.items(length)?;
        let mut values = Vec::with_capacity(length);
        for value in left.iter().chain(right) {
            self.step()?;
            values.push(value.clone());
        }
        Ok(Value::List(values))
    }
    fn count(&mut self, values: &[Value]) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        for _ in values {
            self.step()?;
        }
        Ok(Value::Int(BigInt::from(values.len())))
    }
    fn sum(&mut self, values: &[Value]) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        if values.iter().all(|value| matches!(value, Value::Float(_))) && !values.is_empty() {
            self.step()?;
            let mut total = match values.first() {
                Some(Value::Float(value)) => f64::from_bits(*value),
                _ => unreachable!("non-empty all-float list has a first Float"),
            };
            for value in &values[1..] {
                self.step()?;
                let Value::Float(value) = value else {
                    unreachable!("all-float list was checked above")
                };
                total += f64::from_bits(*value);
            }
            return Ok(Value::Float(if total.is_nan() {
                CANONICAL_NAN_BITS
            } else {
                total.to_bits()
            }));
        }
        if values
            .iter()
            .all(|value| matches!(value, Value::Decimal(_)))
            && !values.is_empty()
        {
            self.step()?;
            let Value::Decimal(first) = &values[0] else {
                unreachable!("non-empty all-Decimal list has a first Decimal")
            };
            let mut total = first.clone();
            for value in &values[1..] {
                self.step()?;
                let Value::Decimal(value) = value else {
                    unreachable!("all-Decimal list was checked above")
                };
                total = total.add(value)?;
            }
            return Ok(Value::Decimal(total));
        }
        if !values.is_empty()
            && values
                .iter()
                .all(|value| matches!(value, Value::Money { .. }))
        {
            self.step()?;
            let Value::Money {
                amount: first_amount,
                currency,
            } = &values[0]
            else {
                unreachable!("non-empty all-Money list has a first Money");
            };
            let mut total = first_amount.clone();
            for value in &values[1..] {
                self.step()?;
                let Value::Money {
                    amount,
                    currency: other_currency,
                } = value
                else {
                    unreachable!("all-Money list was checked above");
                };
                if currency != other_currency {
                    return Err(error("ORNA-EVAL-UNSUPPORTED"));
                }
                total = self.checked_decimal(total.add(amount)?)?;
            }
            return Ok(Value::Money {
                amount: total,
                currency: *currency,
            });
        }
        let mut total = BigInt::ZERO;
        for value in values {
            self.step()?;
            let Value::Int(value) = value else {
                // Mixed numeric kinds, Money and affine quantities remain
                // outside this evaluator slice until their runtime contracts
                // exist.
                return Err(error("ORNA-EVAL-UNSUPPORTED"));
            };
            total = self.integer(&total + value)?;
        }
        Ok(Value::Int(total))
    }
    fn stats_options(&self, values: &[Value]) -> Result<Option<(usize, bool)>, EvaluationError> {
        if values.is_empty() {
            return Ok(None);
        }
        if matches!(values, [Value::Null, Value::Null]) {
            return Ok(None);
        }
        if values.len() != 2 {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        let Value::Int(scale) = &values[0] else {
            return Err(error("ORNA-EVAL-TYPE"));
        };
        if scale.is_negative() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        let scale = scale.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        if scale > self.limits.max_integer_digits {
            return Err(error("ORNA-EVAL-LIMIT"));
        }
        let Value::String(rounding) = &values[1] else {
            return Err(error("ORNA-EVAL-TYPE"));
        };
        if rounding != "half_even" {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        Ok(Some((scale, true)))
    }

    fn exact_decimal_result(
        &self,
        value: DecimalValue,
        preserve_decimal: bool,
    ) -> Result<Value, EvaluationError> {
        let value = self.checked_decimal(value)?;
        if preserve_decimal || value.exponent10.is_negative() {
            return Ok(Value::Decimal(value));
        }
        let power = value
            .exponent10
            .to_usize()
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        let integer = self.integer(value.coefficient * BigInt::from(10u8).pow(power as u32))?;
        Ok(Value::Int(integer))
    }

    fn divide_stats(
        &self,
        left: &DecimalValue,
        right: &DecimalValue,
        options: Option<(usize, bool)>,
        preserve_decimal: bool,
    ) -> Result<Value, EvaluationError> {
        match left.divide(right) {
            Ok(value) => {
                let value = if let Some((scale, _)) = options {
                    value.round_to_scale(scale)?
                } else {
                    value
                };
                self.exact_decimal_result(value, preserve_decimal)
            }
            Err(failure) if matches!(failure.code(), "InexactDivision" | "ORNA-EVAL-VALUE") => {
                let Some((scale, _)) = options else {
                    return Err(error("ORNA-EVAL-VALUE"));
                };
                self.exact_decimal_result(left.divide_rounded(right, scale)?, preserve_decimal)
            }
            Err(failure) => Err(failure),
        }
    }

    fn numeric_decimal(&self, value: &Value) -> Result<(DecimalValue, bool), EvaluationError> {
        match value {
            Value::Int(value) => Ok((DecimalValue::new(value.clone(), BigInt::zero())?, false)),
            Value::Decimal(value) => Ok((value.clone(), true)),
            _ => Err(error("ORNA-EVAL-TYPE")),
        }
    }

    fn variance(
        &mut self,
        values: &[Value],
        options: Option<(usize, bool)>,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let Some(first) = values.first() else {
            return Ok(Value::Null);
        };
        if values.iter().all(|value| matches!(value, Value::Float(_))) {
            let mut sum = 0.0;
            for value in values {
                self.step()?;
                let Value::Float(bits) = value else {
                    unreachable!()
                };
                sum += f64::from_bits(*bits);
            }
            let mean = sum / values.len() as f64;
            let mut squares = 0.0;
            for value in values {
                self.step()?;
                let Value::Float(bits) = value else {
                    unreachable!()
                };
                let delta = f64::from_bits(*bits) - mean;
                squares += delta * delta;
            }
            return finite_float(squares / values.len() as f64);
        }
        if !matches!(first, Value::Int(_) | Value::Decimal(_))
            || !values
                .iter()
                .all(|value| matches!(value, Value::Int(_) | Value::Decimal(_)))
        {
            return Err(error("ORNA-EVAL-TYPE"));
        }
        let preserve_decimal = values
            .iter()
            .any(|value| matches!(value, Value::Decimal(_)));
        let zero = DecimalValue::new(BigInt::zero(), BigInt::zero())?;
        let mut sum = zero.clone();
        let mut squares = zero;
        for value in values {
            self.step()?;
            let (value, _) = self.numeric_decimal(value)?;
            sum = sum.add(&value)?;
            squares = squares.add(&value.multiply(&value)?)?;
        }
        let count = DecimalValue::new(BigInt::from(values.len()), BigInt::zero())?;
        let numerator = squares.multiply(&count)?.add(
            &DecimalValue::new(-sum.coefficient.clone(), sum.exponent10.clone())?.multiply(&sum)?,
        )?;
        let denominator = DecimalValue::new(
            BigInt::from(values.len()) * BigInt::from(values.len()),
            BigInt::zero(),
        )?;
        self.divide_stats(&numerator, &denominator, options, preserve_decimal)
    }

    fn square_root(
        &self,
        value: &DecimalValue,
        options: Option<(usize, bool)>,
        preserve_decimal: bool,
    ) -> Result<Value, EvaluationError> {
        if value.coefficient.is_negative() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        let mut coefficient = value.coefficient.clone();
        let mut exponent = value.exponent10.clone();
        if exponent.is_odd() {
            coefficient *= 10;
            exponent -= 1;
        }
        let root = coefficient.sqrt();
        if &root * &root == coefficient {
            let result = DecimalValue::new(root, exponent / 2)?;
            return self.exact_decimal_result(result, preserve_decimal);
        }
        let Some((scale, _)) = options else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        let scale_i32 = i32::try_from(scale).map_err(|_| error("ORNA-EVAL-LIMIT"))?;
        let shift = value.exponent10.clone() + BigInt::from(scale_i32 * 2);
        let (numerator, denominator) = if shift.is_negative() {
            let power = (-shift)
                .to_u32()
                .filter(|power| *power <= self.limits.max_integer_digits as u32)
                .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
            (value.coefficient.clone(), BigInt::from(10u8).pow(power))
        } else {
            let power = shift
                .to_u32()
                .filter(|power| *power <= self.limits.max_integer_digits as u32)
                .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
            (
                &value.coefficient * BigInt::from(10u8).pow(power),
                BigInt::from(1u8),
            )
        };
        let floor = (&numerator / &denominator).sqrt();
        let exact = &floor * &floor * &denominator == numerator;
        let rounded = if exact {
            floor
        } else {
            let midpoint_twice = &floor * 2 + 1;
            let lhs = &numerator * 4;
            let rhs = &denominator * &midpoint_twice * &midpoint_twice;
            if lhs > rhs || (lhs == rhs && floor.is_odd()) {
                floor + 1
            } else {
                floor
            }
        };
        self.exact_decimal_result(
            DecimalValue::new(rounded, -BigInt::from(scale_i32))?,
            preserve_decimal,
        )
    }

    fn standard_deviation(
        &mut self,
        values: &[Value],
        options: Option<(usize, bool)>,
    ) -> Result<Value, EvaluationError> {
        let variance = self.variance(values, options)?;
        match variance {
            Value::Null => Ok(Value::Null),
            Value::Float(bits) => finite_float(f64::from_bits(bits).sqrt()),
            Value::Int(_) | Value::Decimal(_) => {
                let (variance, preserve_decimal) = self.numeric_decimal(&variance)?;
                self.square_root(&variance, options, preserve_decimal)
            }
            _ => Err(error("ORNA-EVAL-TYPE")),
        }
    }

    fn stats_range(&mut self, values: &[Value]) -> Result<Value, EvaluationError> {
        let low = self.extreme("min", values)?;
        let high = self.extreme("max", values)?;
        let (Value::Option(Some(low)), Value::Option(Some(high))) = (low, high) else {
            return Ok(Value::Null);
        };
        self.apply_binary("-", *high, *low)
    }

    fn stats_mode(&mut self, values: &[Value]) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut counts: Vec<(Value, usize)> = Vec::new();
        for value in values {
            self.step()?;
            if let Some((_, count)) = counts.iter_mut().find(|(current, _)| {
                matches!(
                    self.apply_binary("==", current.clone(), value.clone()),
                    Ok(Value::Bool(true))
                )
            }) {
                *count = count
                    .checked_add(1)
                    .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
            } else {
                counts.push((value.clone(), 1));
                self.items(counts.len())?;
            }
        }
        let max_count = counts.iter().map(|(_, count)| *count).max().unwrap_or(0);
        Ok(Value::List(
            counts
                .into_iter()
                .filter_map(|(value, count)| (count == max_count && max_count > 0).then_some(value))
                .collect(),
        ))
    }

    fn stats_histogram(
        &mut self,
        values: &[Value],
        bins: &[Value],
        include_final_upper: bool,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        self.items(bins.len())?;
        let mut bounds = Vec::with_capacity(bins.len());
        let mut previous_upper: Option<&Value> = None;
        for bin in bins {
            let Value::Tuple(pair) = bin else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            let [lower, upper] = pair.as_slice() else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            let overlaps_previous = if let Some(previous) = previous_upper {
                compare_sort_keys(previous, lower)?.is_gt()
            } else {
                false
            };
            if compare_sort_keys(lower, upper)?.is_ge() || overlaps_previous {
                return Err(error("ORNA-EVAL-VALUE"));
            }
            bounds.push((lower.clone(), upper.clone()));
            previous_upper = Some(upper);
        }
        let mut counts = vec![0usize; bounds.len()];
        for value in values {
            self.step()?;
            for (index, (lower, upper)) in bounds.iter().enumerate() {
                let lower_ok = compare_sort_keys(value, lower)?.is_ge();
                let upper_order = compare_sort_keys(value, upper)?;
                let final_inclusive = include_final_upper && index + 1 == bounds.len();
                if lower_ok && (upper_order.is_lt() || (final_inclusive && upper_order.is_eq())) {
                    counts[index] = counts[index]
                        .checked_add(1)
                        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
                    break;
                }
            }
        }
        Ok(Value::List(
            counts
                .into_iter()
                .map(|count| Value::Int(BigInt::from(count)))
                .collect(),
        ))
    }

    fn elapsed_seconds(&self, start: &Value, end: &Value) -> Result<DecimalValue, EvaluationError> {
        let (
            Value::Instant {
                unix_seconds: start_seconds,
                nanosecond: start_nanos,
            },
            Value::Instant {
                unix_seconds: end_seconds,
                nanosecond: end_nanos,
            },
        ) = (start, end)
        else {
            return Err(error("ORNA-EVAL-TYPE"));
        };
        let nanoseconds = elapsed_total_nanoseconds(&BigInt::from(*end_seconds), *end_nanos)
            - elapsed_total_nanoseconds(&BigInt::from(*start_seconds), *start_nanos);
        if nanoseconds <= BigInt::zero() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        DecimalValue::new(nanoseconds, BigInt::from(-9))
    }

    fn series_points<'a>(
        &self,
        points: &'a [Value],
    ) -> Result<Vec<(&'a Value, &'a Value)>, EvaluationError> {
        let mut result: Vec<(&Value, &Value)> = Vec::with_capacity(points.len());
        for point in points {
            let Value::Tuple(pair) = point else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            let [time, value] = pair.as_slice() else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            if !matches!(time, Value::Instant { .. }) {
                return Err(error("ORNA-EVAL-TYPE"));
            }
            if let Some((previous, _)) = result.last()
                && compare_values(previous, time)?.is_ge()
            {
                return Err(error("ORNA-EVAL-VALUE"));
            }
            result.push((time, value));
        }
        if result.len() < 2 {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        Ok(result)
    }

    fn series_slope(
        &self,
        start_time: &Value,
        start_value: &Value,
        end_time: &Value,
        end_value: &Value,
    ) -> Result<Value, EvaluationError> {
        let elapsed = self.elapsed_seconds(start_time, end_time)?;
        match (start_value, end_value) {
            (Value::Float(start), Value::Float(end)) => finite_float(
                (f64::from_bits(*end) - f64::from_bits(*start)) / decimal_to_f64(&elapsed)?,
            ),
            (Value::Int(_) | Value::Decimal(_), Value::Int(_) | Value::Decimal(_)) => {
                let (start, start_decimal) = self.numeric_decimal(start_value)?;
                let (end, end_decimal) = self.numeric_decimal(end_value)?;
                let delta = end.add(&DecimalValue::new(-start.coefficient, start.exponent10)?)?;
                self.divide_stats(&delta, &elapsed, None, start_decimal || end_decimal)
            }
            _ => Err(error("ORNA-EVAL-TYPE")),
        }
    }

    fn stats_rate(&self, points: &[Value]) -> Result<Value, EvaluationError> {
        let points = self.series_points(points)?;
        let (start_time, start_value) = points[0];
        let (end_time, end_value) = points[points.len() - 1];
        self.series_slope(start_time, start_value, end_time, end_value)
    }

    fn stats_derivative(&mut self, points: &[Value]) -> Result<Value, EvaluationError> {
        let points = self.series_points(points)?;
        let mut derivatives = Vec::with_capacity(points.len() - 1);
        for pair in points.windows(2) {
            self.step()?;
            derivatives.push(Value::Tuple(vec![
                pair[1].0.clone(),
                self.series_slope(pair[0].0, pair[0].1, pair[1].0, pair[1].1)?,
            ]));
            self.items(derivatives.len())?;
        }
        Ok(Value::List(derivatives))
    }

    fn stats_integrate(&self, points: &[Value]) -> Result<Value, EvaluationError> {
        let points = self.series_points(points)?;
        if points
            .iter()
            .all(|(_, value)| matches!(value, Value::Float(_)))
        {
            let mut total = 0.0;
            for pair in points.windows(2) {
                let (Value::Float(left), Value::Float(right)) = (pair[0].1, pair[1].1) else {
                    unreachable!()
                };
                let elapsed = decimal_to_f64(&self.elapsed_seconds(pair[0].0, pair[1].0)?)?;
                total += (f64::from_bits(*left) + f64::from_bits(*right)) * 0.5 * elapsed;
            }
            return finite_float(total);
        }
        let mut total = DecimalValue::new(BigInt::zero(), BigInt::zero())?;
        let mut preserve_decimal = false;
        for pair in points.windows(2) {
            let (left, left_decimal) = self.numeric_decimal(pair[0].1)?;
            let (right, right_decimal) = self.numeric_decimal(pair[1].1)?;
            preserve_decimal |= left_decimal || right_decimal;
            let sum = left.add(&right)?;
            let average = self.divide_stats(
                &sum,
                &DecimalValue::new(BigInt::from(2), BigInt::zero())?,
                None,
                preserve_decimal,
            )?;
            let (average, average_decimal) = self.numeric_decimal(&average)?;
            preserve_decimal |= average_decimal;
            let area = average.multiply(&self.elapsed_seconds(pair[0].0, pair[1].0)?)?;
            total = total.add(&area)?;
        }
        self.exact_decimal_result(total, preserve_decimal)
    }

    fn stats(&mut self, name: &str, values: Vec<Value>) -> Result<Value, EvaluationError> {
        let name = name.strip_prefix("__").unwrap_or(name);
        match name {
            "mean" | "median" | "variance" | "standard_deviation" => {
                let [Value::List(rows), rest @ ..] = values.as_slice() else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                let options = self.stats_options(rest)?;
                self.items(rows.len())?;
                if rows.is_empty() {
                    return Ok(Value::Null);
                }
                match name {
                    "mean" => self.mean(rows, options),
                    "median" => self.median(rows, options),
                    "variance" => self.variance(rows, options),
                    "standard_deviation" => self.standard_deviation(rows, options),
                    _ => unreachable!(),
                }
            }
            "percentile" => {
                let [
                    Value::List(rows),
                    probability,
                    Value::String(interpolation),
                    rest @ ..,
                ] = values.as_slice()
                else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                self.percentile(rows, probability, interpolation, self.stats_options(rest)?)
            }
            "sum" => {
                let [Value::List(rows)] = values.as_slice() else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                self.sum(rows)
            }
            "min" | "max" => {
                let [Value::List(rows)] = values.as_slice() else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                self.extreme(name, rows)
            }
            "range" => {
                let [Value::List(rows)] = values.as_slice() else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                self.stats_range(rows)
            }
            "mode" => {
                let [Value::List(rows)] = values.as_slice() else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                self.stats_mode(rows)
            }
            "histogram" => {
                let [
                    Value::List(rows),
                    Value::List(bins),
                    Value::Bool(include_final_upper),
                ] = values.as_slice()
                else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                self.stats_histogram(rows, bins, *include_final_upper)
            }
            "rate" | "derivative" | "integrate" => {
                let [Value::List(points)] = values.as_slice() else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                match name {
                    "rate" => self.stats_rate(points),
                    "derivative" => self.stats_derivative(points),
                    "integrate" => self.stats_integrate(points),
                    _ => unreachable!(),
                }
            }
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }

    fn mean(
        &self,
        values: &[Value],
        options: Option<(usize, bool)>,
    ) -> Result<Value, EvaluationError> {
        let count = DecimalValue::new(BigInt::from(values.len()), BigInt::zero())?;
        match values.first() {
            Some(Value::Int(_)) if values.iter().all(|value| matches!(value, Value::Int(_))) => {
                let mut total = BigInt::zero();
                for value in values {
                    let Value::Int(value) = value else {
                        unreachable!()
                    };
                    total = self.integer(total + value)?;
                }
                self.divide_stats(
                    &DecimalValue::new(total, BigInt::zero())?,
                    &count,
                    options,
                    false,
                )
            }
            Some(Value::Decimal(_))
                if values
                    .iter()
                    .all(|value| matches!(value, Value::Decimal(_))) =>
            {
                let Value::Decimal(first) = &values[0] else {
                    unreachable!()
                };
                let mut total = first.clone();
                for value in &values[1..] {
                    let Value::Decimal(value) = value else {
                        unreachable!()
                    };
                    total = total.add(value)?;
                }
                self.divide_stats(&total, &count, options, true)
            }
            Some(Value::Float(_))
                if values.iter().all(|value| matches!(value, Value::Float(_))) =>
            {
                let Value::Float(first) = values.first().expect("non-empty Float mean") else {
                    unreachable!("Float branch requires a Float first value")
                };
                let mut total = f64::from_bits(*first);
                for value in &values[1..] {
                    let Value::Float(bits) = value else {
                        unreachable!()
                    };
                    total += f64::from_bits(*bits);
                }
                finite_float(total / values.len() as f64)
            }
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }

    fn median(
        &mut self,
        values: &[Value],
        options: Option<(usize, bool)>,
    ) -> Result<Value, EvaluationError> {
        let Some(first) = values.first() else {
            return Ok(Value::Null);
        };
        let homogeneous = match first {
            Value::Int(_) => values.iter().all(|value| matches!(value, Value::Int(_))),
            Value::Decimal(_) => values
                .iter()
                .all(|value| matches!(value, Value::Decimal(_))),
            Value::Float(_) => values.iter().all(|value| matches!(value, Value::Float(_))),
            _ => false,
        };
        if !homogeneous {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        let mut sorted = values.to_vec();
        for index in 1..sorted.len() {
            let mut current = index;
            while current > 0 {
                self.step()?;
                if compare_sort_keys(&sorted[current - 1], &sorted[current])?
                    != std::cmp::Ordering::Greater
                {
                    break;
                }
                sorted.swap(current - 1, current);
                current -= 1;
            }
        }
        if sorted.len() % 2 == 1 {
            return Ok(sorted[sorted.len() / 2].clone());
        }
        self.average_values(
            &sorted[sorted.len() / 2 - 1],
            &sorted[sorted.len() / 2],
            options,
        )
    }

    fn average_values(
        &self,
        left: &Value,
        right: &Value,
        options: Option<(usize, bool)>,
    ) -> Result<Value, EvaluationError> {
        match (left, right) {
            (Value::Int(left), Value::Int(right)) => {
                let total = self.integer(left + right)?;
                self.divide_stats(
                    &DecimalValue::new(total, BigInt::zero())?,
                    &DecimalValue::new(2.into(), BigInt::zero())?,
                    options,
                    false,
                )
            }
            (Value::Decimal(left), Value::Decimal(right)) => {
                let total = left.add(right)?;
                self.divide_stats(
                    &total,
                    &DecimalValue::new(2.into(), BigInt::zero())?,
                    options,
                    true,
                )
            }
            (Value::Float(left), Value::Float(right)) => {
                finite_float((f64::from_bits(*left) + f64::from_bits(*right)) / 2.0)
            }
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }

    fn percentile(
        &self,
        values: &[Value],
        probability: &Value,
        interpolation: &str,
        options: Option<(usize, bool)>,
    ) -> Result<Value, EvaluationError> {
        if !matches!(
            interpolation,
            "linear" | "lower" | "higher" | "nearest" | "midpoint"
        ) {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        self.items(values.len())?;
        let probability = match probability {
            Value::Int(value) if !value.is_negative() => {
                DecimalValue::new(value.clone(), BigInt::zero())?
            }
            Value::Decimal(value) => value.clone(),
            Value::Float(value) if f64::from_bits(*value).is_finite() => {
                if !((0.0..=1.0).contains(&f64::from_bits(*value))) {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                return self.percentile_float(values, f64::from_bits(*value), interpolation);
            }
            _ => return Err(error("ORNA-EVAL-VALUE")),
        };
        let zero = DecimalValue::new(BigInt::zero(), BigInt::zero())?;
        let one = DecimalValue::new(BigInt::from(1u8), BigInt::zero())?;
        if compare_values(&Value::Decimal(probability.clone()), &Value::Decimal(zero))?
            == std::cmp::Ordering::Less
            || compare_values(&Value::Decimal(probability.clone()), &Value::Decimal(one))?
                == std::cmp::Ordering::Greater
        {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        if values.is_empty() {
            return Ok(Value::Null);
        }
        let Some(first) = values.first() else {
            return Ok(Value::Null);
        };
        let homogeneous = match first {
            Value::Int(_) => values.iter().all(|value| matches!(value, Value::Int(_))),
            Value::Decimal(_) => values
                .iter()
                .all(|value| matches!(value, Value::Decimal(_))),
            Value::Float(_) => values.iter().all(|value| matches!(value, Value::Float(_))),
            _ => false,
        };
        if !homogeneous {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        let mut sorted = values.to_vec();
        for index in 1..sorted.len() {
            let mut current = index;
            while current > 0
                && compare_sort_keys(&sorted[current - 1], &sorted[current])?
                    == std::cmp::Ordering::Greater
            {
                sorted.swap(current - 1, current);
                current -= 1;
            }
        }
        let values = sorted.as_slice();
        let span = DecimalValue::new(BigInt::from(values.len() - 1), BigInt::zero())?;
        let position = probability.multiply(&span)?;
        let (index, fraction) = decimal_floor_fraction(&position)?;
        let lower = values.get(index).ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
        let upper = values.get(index.saturating_add(1)).unwrap_or(lower);
        match interpolation {
            "lower" => Ok(lower.clone()),
            "higher" => {
                if fraction.coefficient.is_zero() {
                    Ok(lower.clone())
                } else {
                    Ok(upper.clone())
                }
            }
            "nearest" => {
                let half = DecimalValue::new(5.into(), (-1).into())?;
                if compare_values(&Value::Decimal(fraction), &Value::Decimal(half))?
                    == std::cmp::Ordering::Less
                {
                    Ok(lower.clone())
                } else {
                    Ok(upper.clone())
                }
            }
            "midpoint" if fraction.coefficient.is_zero() => Ok(lower.clone()),
            "midpoint" => self.average_values(lower, upper, options),
            "linear" if fraction.coefficient.is_zero() => Ok(lower.clone()),
            "linear" => self.interpolate_exact(lower, upper, &fraction, options),
            _ => Err(error("ORNA-EVAL-VALUE")),
        }
    }

    fn interpolate_exact(
        &self,
        lower: &Value,
        upper: &Value,
        fraction: &DecimalValue,
        options: Option<(usize, bool)>,
    ) -> Result<Value, EvaluationError> {
        match (lower, upper) {
            (Value::Int(lower), Value::Int(upper)) => {
                let low = DecimalValue::new(lower.clone(), BigInt::zero())?;
                let high = DecimalValue::new(upper.clone(), BigInt::zero())?;
                let delta = high.add(&DecimalValue::new(-lower.clone(), BigInt::zero())?)?;
                let result = low.add(&delta.multiply(fraction)?)?;
                let result = if let Some((scale, _)) = options {
                    result.round_to_scale(scale)?
                } else {
                    result
                };
                self.exact_decimal_result(result, false)
            }
            (Value::Decimal(lower), Value::Decimal(upper)) => {
                let delta = upper.add(&DecimalValue::new(
                    -lower.coefficient.clone(),
                    lower.exponent10.clone(),
                )?)?;
                let result = lower.add(&delta.multiply(fraction)?)?;
                let result = if let Some((scale, _)) = options {
                    result.round_to_scale(scale)?
                } else {
                    result
                };
                self.exact_decimal_result(result, true)
            }
            (Value::Float(lower), Value::Float(upper)) => finite_float(
                f64::from_bits(*lower)
                    + (f64::from_bits(*upper) - f64::from_bits(*lower)) * decimal_to_f64(fraction)?,
            ),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }

    fn percentile_float(
        &self,
        values: &[Value],
        probability: f64,
        interpolation: &str,
    ) -> Result<Value, EvaluationError> {
        if values.is_empty() {
            return Ok(Value::Null);
        }
        if !values.iter().all(|value| matches!(value, Value::Float(_))) {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        let mut sorted = values
            .iter()
            .map(|value| match value {
                Value::Float(bits) => *bits,
                _ => unreachable!(),
            })
            .collect::<Vec<_>>();
        sorted.sort_by(|left, right| float_total_cmp(*left, *right));
        let position = probability * (sorted.len() - 1) as f64;
        let lower = position.floor() as usize;
        let upper = position.ceil() as usize;
        let fraction = position - lower as f64;
        let choose = match interpolation {
            "lower" => lower,
            "higher" => upper,
            "nearest" => {
                if fraction < 0.5 {
                    lower
                } else {
                    upper
                }
            }
            _ => lower,
        };
        if interpolation == "linear" {
            return finite_float(
                f64::from_bits(sorted[lower])
                    + (f64::from_bits(sorted[upper]) - f64::from_bits(sorted[lower])) * fraction,
            );
        }
        if interpolation == "midpoint" {
            return finite_float(
                (f64::from_bits(sorted[lower]) + f64::from_bits(sorted[upper])) / 2.0,
            );
        }
        Ok(Value::Float(sorted[choose]))
    }
    fn extreme(&mut self, name: &str, values: &[Value]) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        if values.iter().all(|value| matches!(value, Value::Float(_))) {
            for _ in values {
                self.step()?;
            }
            let bits = values
                .iter()
                .map(|value| match value {
                    Value::Float(bits) => *bits,
                    _ => unreachable!("all-float list was checked above"),
                })
                .collect::<Vec<_>>();
            let result = if name == "min" {
                float_min(&bits)
            } else {
                float_max(&bits)
            };
            return Ok(result.map_or(Value::Null, |value| {
                Value::Option(Some(Box::new(Value::Float(value))))
            }));
        }
        if !values.is_empty()
            && values
                .iter()
                .all(|value| matches!(value, Value::Decimal(_)))
        {
            let mut candidate = None;
            for value in values {
                self.step()?;
                let replace = match candidate.as_ref() {
                    None => true,
                    Some(current) => {
                        let ordering = compare_values(value, current)?;
                        if name == "min" {
                            ordering.is_lt()
                        } else {
                            ordering.is_gt()
                        }
                    }
                };
                if replace {
                    // Strict comparison retains the first equal Decimal,
                    // including values that differ only by representational scale.
                    candidate = Some(value.clone());
                }
            }
            return Ok(candidate.map_or(Value::Null, |value| Value::Option(Some(Box::new(value)))));
        }
        if !values.is_empty()
            && values
                .iter()
                .all(|value| matches!(value, Value::Money { .. }))
        {
            let currency = match &values[0] {
                Value::Money { currency, .. } => *currency,
                _ => unreachable!("non-empty all-Money list has a first Money"),
            };
            let mut candidate = None;
            for value in values {
                self.step()?;
                let Value::Money {
                    currency: value_currency,
                    ..
                } = value
                else {
                    unreachable!("all-Money list was checked above");
                };
                if *value_currency != currency {
                    return Err(error("ORNA-EVAL-UNSUPPORTED"));
                }
                let replace = match candidate.as_ref() {
                    None => true,
                    Some(current) => {
                        let ordering = compare_values(value, current)?;
                        if name == "min" {
                            ordering.is_lt()
                        } else {
                            ordering.is_gt()
                        }
                    }
                };
                if replace {
                    candidate = Some(value.clone());
                }
            }
            return Ok(candidate.map_or(Value::Null, |value| Value::Option(Some(Box::new(value)))));
        }
        let temporal_kind = values.first().and_then(range_endpoint_kind);
        if matches!(temporal_kind, Some("Date" | "Instant" | "Duration"))
            && values
                .iter()
                .all(|value| range_endpoint_kind(value) == temporal_kind)
        {
            let mut candidate = None;
            for value in values {
                self.step()?;
                let replace = match candidate.as_ref() {
                    None => true,
                    Some(current) => {
                        let ordering = compare_values(value, current)?;
                        if name == "min" {
                            ordering.is_lt()
                        } else {
                            ordering.is_gt()
                        }
                    }
                };
                if replace {
                    // Strict comparison retains the first equal temporal
                    // value, including equivalent Instant offsets.
                    candidate = Some(value.clone());
                }
            }
            return Ok(candidate.map_or(Value::Null, |value| Value::Option(Some(Box::new(value)))));
        }
        let mut candidate: Option<Value> = None;
        for value in values {
            self.step()?;
            lawful_sort_key(value)?;
            let replace = match candidate.as_ref() {
                None => true,
                Some(current) => {
                    let ordering = compare_sort_keys(value, current)?;
                    if name == "min" {
                        ordering.is_lt()
                    } else {
                        ordering.is_gt()
                    }
                }
            };
            if replace {
                // Strict comparison above deliberately retains the first
                // equal candidate, preserving observable input order.
                candidate = Some(value.clone());
            }
        }
        Ok(candidate.map_or(Value::Null, |value| Value::Option(Some(Box::new(value)))))
    }
    fn first(&self, values: &[Value]) -> Result<Value, EvaluationError> {
        Ok(values
            .first()
            .cloned()
            .map_or(Value::Null, |value| Value::Option(Some(Box::new(value)))))
    }
    fn last(&self, values: &[Value]) -> Result<Value, EvaluationError> {
        Ok(Value::Option(values.last().cloned().map(Box::new)))
    }
    fn one(
        &mut self,
        values: &[Value],
        predicate: Option<&Value>,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let Some(predicate) = predicate else {
            return match values {
                [] => Err(error("ORNA-EVAL-RELATION-ONE-ZERO")),
                [value] => Ok(value.clone()),
                _ => Err(error("ORNA-EVAL-RELATION-ONE-MULTIPLE")),
            };
        };

        let mut matching = None;
        for value in values {
            self.step()?;
            match self.invoke_predicate(predicate, value.clone(), depth + 1)? {
                Value::Bool(true) => {
                    if matching.is_some() {
                        // The second matching value is sufficient to classify
                        // the cardinality failure; do not invoke the callback
                        // for any later input value.
                        return Err(error("ORNA-EVAL-RELATION-ONE-MULTIPLE"));
                    }
                    matching = Some(value.clone());
                }
                Value::Bool(false) => {}
                _ => return Err(error("ORNA-EVAL-TYPE")),
            }
        }
        matching.ok_or_else(|| error("ORNA-EVAL-RELATION-ONE-ZERO"))
    }
    fn every(
        &mut self,
        values: &[Value],
        predicate: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        for value in values {
            self.step()?;
            match self.invoke_predicate(predicate, value.clone(), depth + 1)? {
                Value::Bool(true) => {}
                Value::Bool(false) => return Ok(Value::Bool(false)),
                _ => return Err(error("ORNA-EVAL-TYPE")),
            }
        }
        Ok(Value::Bool(true))
    }
    fn exists(
        &mut self,
        values: &[Value],
        predicate: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        for value in values {
            self.step()?;
            match self.invoke_predicate(predicate, value.clone(), depth + 1)? {
                Value::Bool(true) => return Ok(Value::Bool(true)),
                Value::Bool(false) => {}
                _ => return Err(error("ORNA-EVAL-TYPE")),
            }
        }
        Ok(Value::Bool(false))
    }
    fn take(&mut self, values: &[Value], count: &BigInt) -> Result<Value, EvaluationError> {
        if count.is_negative() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        let end = if count >= &BigInt::from(values.len()) {
            values.len()
        } else {
            count.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?
        };
        self.items(end)?;
        let mut result = Vec::with_capacity(end);
        for value in &values[..end] {
            self.step()?;
            result.push(value.clone());
        }
        Ok(Value::List(result))
    }
    fn drop(&mut self, values: &[Value], count: &BigInt) -> Result<Value, EvaluationError> {
        if count.is_negative() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        let start = if count >= &BigInt::from(values.len()) {
            values.len()
        } else {
            count.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?
        };
        let result_len = values.len() - start;
        self.items(result_len)?;
        let mut result = Vec::with_capacity(result_len);
        for value in &values[start..] {
            self.step()?;
            result.push(value.clone());
        }
        Ok(Value::List(result))
    }
    fn filter(
        &mut self,
        values: &[Value],
        predicate: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut filtered = Vec::new();
        for value in values {
            // Invoke once, in input order, and retain only an explicit true.
            self.step()?;
            match self.invoke_predicate(predicate, value.clone(), depth + 1)? {
                Value::Bool(true) => {
                    filtered.push(value.clone());
                    self.items(filtered.len())?;
                }
                Value::Bool(false) => {}
                _ => return Err(error("ORNA-EVAL-TYPE")),
            }
        }
        Ok(Value::List(filtered))
    }
    fn partition(
        &mut self,
        values: &[Value],
        predicate: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        self.items(2)?;
        let mut matching = Vec::new();
        let mut remaining = Vec::new();
        for value in values {
            // Invoke once, in input order, so a lawful callback's observable
            // behavior is never duplicated by classification.
            self.step()?;
            match self.invoke_predicate(predicate, value.clone(), depth + 1)? {
                Value::Bool(true) => {
                    matching.push(value.clone());
                    self.items(matching.len())?;
                }
                Value::Bool(false) => {
                    remaining.push(value.clone());
                    self.items(remaining.len())?;
                }
                _ => return Err(error("ORNA-EVAL-TYPE")),
            }
        }
        Ok(Value::Tuple(vec![
            Value::List(matching),
            Value::List(remaining),
        ]))
    }
    fn split_when(
        &mut self,
        values: &[Value],
        predicate: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut groups = Vec::new();
        let mut current = Vec::new();
        for value in values {
            // Invoke once, in input order. A true boundary starts the next
            // group, except at the beginning where no empty group is lawful.
            self.step()?;
            match self.invoke_predicate(predicate, value.clone(), depth + 1)? {
                Value::Bool(true) if !current.is_empty() => {
                    groups.push(Value::List(std::mem::take(&mut current)));
                    self.items(groups.len())?;
                }
                Value::Bool(_) => {}
                _ => return Err(error("ORNA-EVAL-TYPE")),
            }
            current.push(value.clone());
            self.items(current.len())?;
        }
        if !current.is_empty() {
            groups.push(Value::List(current));
            self.items(groups.len())?;
        }
        Ok(Value::List(groups))
    }
    fn group_by(
        &mut self,
        values: &[Value],
        key: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut groups = Vec::<(Value, Vec<Value>)>::new();
        for value in values {
            // Classify once, in input order. The returned key must have an
            // explicit total comparison; canonical encoding alone is not a
            // substitute for the library's lawful-key requirement.
            self.step()?;
            let group_key = self.invoke_predicate(key, value.clone(), depth + 1)?;
            lawful_group_key(&group_key)?;
            let mut matched = false;
            for (existing_key, rows) in &mut groups {
                if compare_group_keys(existing_key, &group_key)? == std::cmp::Ordering::Equal {
                    rows.push(value.clone());
                    self.items(rows.len())?;
                    matched = true;
                    break;
                }
            }
            if !matched {
                self.items(groups.len() + 1)?;
                groups.push((group_key, vec![value.clone()]));
            }
        }

        // Insertion sort keeps the ordering decision fallible and avoids
        // accepting an incidental host ordering for source values.
        for index in 1..groups.len() {
            let mut current = index;
            while current > 0
                && compare_group_keys(&groups[current - 1].0, &groups[current].0)?
                    == std::cmp::Ordering::Greater
            {
                groups.swap(current - 1, current);
                current -= 1;
            }
        }
        self.items(groups.len())?;
        Ok(Value::List(
            groups
                .into_iter()
                .map(|(key, rows)| Value::Tuple(vec![key, Value::List(rows)]))
                .collect(),
        ))
    }
    fn asof_join(
        &mut self,
        left: &[Value],
        right: &[Value],
        time: &Value,
        by: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        // Selectors are applied to finite rows, and results retain left input
        // order as `(left, right-or-null)` tuples. For each row, only right
        // timestamps at or before the left timestamp are eligible. The latest
        // eligible timestamp wins; canonical row bytes break equal-time ties.
        self.items(left.len())?;
        self.items(right.len())?;
        if left.is_empty() {
            return Ok(Value::List(Vec::new()));
        }

        let mut left_keys = Vec::with_capacity(left.len());
        for row in left {
            self.step()?;
            let time_key = self.invoke_predicate(time, row.clone(), depth + 1)?;
            lawful_asof_time(&time_key)?;
            self.step()?;
            let group_key = self.invoke_predicate(by, row.clone(), depth + 1)?;
            lawful_group_key(&group_key)?;
            left_keys.push((time_key, group_key));
            self.items(left_keys.len())?;
        }

        let mut right_keys = Vec::with_capacity(right.len());
        for row in right {
            self.step()?;
            let time_key = self.invoke_predicate(time, row.clone(), depth + 1)?;
            lawful_asof_time(&time_key)?;
            self.step()?;
            let group_key = self.invoke_predicate(by, row.clone(), depth + 1)?;
            lawful_group_key(&group_key)?;
            let canonical_key = row
                .clone()
                .canonical()?
                .encode()
                .map_err(|_| error("ORNA-EVAL-VALUE"))?;
            right_keys.push((time_key, group_key, canonical_key));
            self.items(right_keys.len())?;
        }

        let mut joined = Vec::with_capacity(left.len());
        for (left_row, (left_time, left_group)) in left.iter().zip(left_keys) {
            let mut selected = None::<(Value, Vec<u8>, Value)>;
            for (right_row, (right_time, right_group, canonical_key)) in
                right.iter().zip(&right_keys)
            {
                self.step()?;
                if compare_group_keys(&left_group, right_group)? != std::cmp::Ordering::Equal {
                    continue;
                }
                if compare_values(right_time, &left_time)? == std::cmp::Ordering::Greater {
                    continue;
                }
                let replace = match &selected {
                    None => true,
                    Some((best_time, best_key, _)) => {
                        match compare_values(right_time, best_time)? {
                            std::cmp::Ordering::Greater => true,
                            std::cmp::Ordering::Equal => canonical_key > best_key,
                            std::cmp::Ordering::Less => false,
                        }
                    }
                };
                if replace {
                    selected = Some((right_time.clone(), canonical_key.clone(), right_row.clone()));
                }
            }
            joined.push(Value::Tuple(vec![
                left_row.clone(),
                selected.map_or(Value::Null, |(_, _, row)| {
                    Value::Option(Some(Box::new(row)))
                }),
            ]));
            self.items(joined.len())?;
        }
        Ok(Value::List(joined))
    }
    fn invoke_callable(
        &mut self,
        callable: &Value,
        arguments: Vec<Value>,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        let (parameters, body, mut captured, session_owned, namespace) = match callable {
            Value::Function {
                name,
                nominal_definitions,
            } => {
                let function = self
                    .functions
                    .get(name)
                    .ok_or_else(|| error("ORNA-EVAL-NAME"))?;
                (
                    function.parameters.clone(),
                    function.body.clone(),
                    Scope::from_environment_with_nominals(
                        &function.environment,
                        nominal_definitions,
                        self,
                    )?,
                    self.session_functions
                        .is_some_and(|functions| functions.contains(name)),
                    function_namespace(name),
                )
            }
            Value::Closure(closure) => (
                closure.parameters.clone(),
                closure.body.clone(),
                closure.captured.clone(),
                self.repl_bindings,
                closure.namespace.clone(),
            ),
            _ => return Err(error("ORNA-EVAL-TYPE")),
        };
        self.depth(depth + 1)?;
        self.items(arguments.len())?;
        if arguments.len() > parameters.len() {
            return Err(error("ORNA-EVAL-ARGUMENT"));
        }
        captured.2.extend(
            self.session_functions
                .into_iter()
                .flat_map(|functions| functions.iter().cloned())
                .filter(|name| captured.0.contains_key(name)),
        );
        let mut supplied = BTreeMap::new();
        for (index, value) in arguments.into_iter().enumerate() {
            supplied.insert(parameter_key(&parameters[index], index), value);
        }
        let previous_repl_bindings = self.repl_bindings;
        self.repl_bindings = session_owned;
        let previous_namespace = self.namespace.clone();
        self.namespace = namespace;
        let result = invoke_pure(self, &parameters, &body, captured, supplied, depth + 1);
        self.namespace = previous_namespace;
        self.repl_bindings = previous_repl_bindings;
        result
    }
    fn invoke_predicate(
        &mut self,
        callable: &Value,
        input: Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        let (parameters, body, captured, session_owned, namespace) = match callable {
            Value::Function {
                name,
                nominal_definitions,
            } => {
                let function = self
                    .functions
                    .get(name)
                    .ok_or_else(|| error("ORNA-EVAL-NAME"))?;
                (
                    function.parameters.clone(),
                    function.body.clone(),
                    Scope::from_environment_with_nominals(
                        &function.environment,
                        nominal_definitions,
                        self,
                    )?,
                    self.session_functions
                        .is_some_and(|functions| functions.contains(name)),
                    function_namespace(name),
                )
            }
            Value::Closure(closure) => (
                closure.parameters.clone(),
                closure.body.clone(),
                closure.captured.clone(),
                self.repl_bindings,
                closure.namespace.clone(),
            ),
            _ => return Err(error("ORNA-EVAL-TYPE")),
        };
        self.depth(depth + 1)?;
        self.items(1)?;
        let Some(parameter) = parameters.first() else {
            return Err(error("ORNA-EVAL-ARGUMENT"));
        };
        let mut supplied = BTreeMap::new();
        supplied.insert(parameter_key(parameter, 0), input);
        let previous_repl_bindings = self.repl_bindings;
        self.repl_bindings = session_owned;
        let previous_namespace = self.namespace.clone();
        self.namespace = namespace;
        let result = invoke_pure(self, &parameters, &body, captured, supplied, depth + 1);
        self.namespace = previous_namespace;
        self.repl_bindings = previous_repl_bindings;
        result
    }
    fn positive_collection_size(&self, value: &BigInt) -> Result<usize, EvaluationError> {
        if !value.is_positive() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        Ok(value.to_usize().unwrap_or(usize::MAX))
    }
    fn zipped(
        &mut self,
        left: &[Value],
        right: &[Value],
        exact: bool,
    ) -> Result<Value, EvaluationError> {
        self.items(left.len())?;
        self.items(right.len())?;
        if exact && left.len() != right.len() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        let mut pairs = Vec::new();
        for (left, right) in left.iter().zip(right) {
            self.items(2)?;
            self.step()?;
            pairs.push(Value::Tuple(vec![left.clone(), right.clone()]));
            self.items(pairs.len())?;
        }
        Ok(Value::List(pairs))
    }
    fn windows(
        &mut self,
        values: &[Value],
        size: &BigInt,
        step: &BigInt,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let size = self.positive_collection_size(size)?;
        let step = self.positive_collection_size(step)?;
        if size > values.len() {
            return Ok(Value::List(Vec::new()));
        }
        for _ in values {
            self.step()?;
        }
        let last_start = values.len() - size;
        let mut start = 0;
        let mut windows = Vec::new();
        while start <= last_start {
            let window = &values[start..start + size];
            self.items(window.len())?;
            self.step()?;
            windows.push(Value::List(window.to_vec()));
            self.items(windows.len())?;
            let Some(next) = start.checked_add(step) else {
                break;
            };
            start = next;
        }
        Ok(Value::List(windows))
    }
    fn strictly_ordered_time_series(&mut self, points: &[Value]) -> Result<Value, EvaluationError> {
        self.items(points.len())?;
        let mut previous_time: Option<&Value> = None;
        for point in points {
            self.step()?;
            let Value::Tuple(pair) = point else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            let [time, _] = pair.as_slice() else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            if !matches!(time, Value::Instant { .. }) {
                return Err(error("ORNA-EVAL-TYPE"));
            }
            if let Some(previous_time) = previous_time
                && compare_values(previous_time, time)?.is_ge()
            {
                return Ok(Value::Bool(false));
            }
            previous_time = Some(time);
        }
        Ok(Value::Bool(true))
    }
    fn window_time_series_statistics(
        &mut self,
        points: &[Value],
        size: &BigInt,
        step: &BigInt,
        statistic: &str,
    ) -> Result<Value, EvaluationError> {
        // Visit complete windows in source order and discard each temporary
        // window after computing its statistic. This keeps sparse timestamp
        // order attached to each result without retaining every overlapping
        // window at once.
        self.items(points.len())?;
        let size = self.positive_collection_size(size)?;
        let step = self.positive_collection_size(step)?;
        if size > points.len() {
            return Ok(Value::List(Vec::new()));
        }
        for _ in points {
            self.step()?;
        }

        let last_start = points.len() - size;
        let mut start = 0usize;
        let mut results = Vec::new();
        while start <= last_start {
            let end = start + size;
            self.items(size)?;
            self.step()?;
            let window = points[start..end].to_vec();
            self.step()?;
            let result = if statistic == "rate_integrate" {
                let rate = self.stats_rate(&window)?;
                let integral = self.stats_integrate(&window)?;
                Value::Tuple(vec![
                    Value::Option(Some(Box::new(rate))),
                    Value::Option(Some(Box::new(integral))),
                ])
            } else {
                let result = self.stats(statistic, vec![Value::List(window)])?;
                if matches!(statistic, "rate" | "integrate") {
                    Value::Option(Some(Box::new(result)))
                } else {
                    result
                }
            };
            results.push(result);
            self.items(results.len())?;

            let Some(next) = start.checked_add(step) else {
                break;
            };
            start = next;
        }
        Ok(Value::List(results))
    }
    fn shift_left(&self, value: &BigInt, count: &BigInt) -> Result<Value, EvaluationError> {
        if count.is_negative() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        if value.is_zero() {
            return Ok(Value::Int(BigInt::ZERO));
        }
        let max_bits = self.limits.max_integer_digits.saturating_mul(4);
        if BigInt::from(value.bits()) + count > BigInt::from(max_bits) {
            return Err(error("ORNA-EVAL-LIMIT"));
        }
        let count = count.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        Ok(Value::Int(self.integer(value << count)?))
    }
    fn shift_right(&self, value: &BigInt, count: &BigInt) -> Result<Value, EvaluationError> {
        if count.is_negative() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        if count >= &BigInt::from(value.bits()) {
            return Ok(Value::Int(if value.is_negative() {
                BigInt::from(-1)
            } else {
                BigInt::ZERO
            }));
        }
        let count = count.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        Ok(Value::Int(self.integer(value >> count)?))
    }
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn encode_base64(input: &[u8]) -> String {
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or_default();
        let c = chunk.get(2).copied().unwrap_or_default();
        output.push(BASE64_ALPHABET[usize::from(a >> 2)] as char);
        output.push(BASE64_ALPHABET[usize::from(((a & 0x03) << 4) | (b >> 4))] as char);
        output.push(if chunk.len() > 1 {
            BASE64_ALPHABET[usize::from(((b & 0x0f) << 2) | (c >> 6))] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            BASE64_ALPHABET[usize::from(c & 0x3f)] as char
        } else {
            '='
        });
    }
    output
}

fn decode_base64(input: &str) -> Result<Vec<u8>, EvaluationError> {
    if !input.is_ascii() || input.len() % 4 != 0 {
        return Err(error("ORNA-EVAL-VALUE"));
    }
    let mut output = Vec::with_capacity(input.len() / 4 * 3);
    let chunks = input.as_bytes().chunks_exact(4);
    let count = chunks.len();
    for (index, chunk) in chunks.enumerate() {
        let final_chunk = index + 1 == count;
        let decode = |byte: u8| {
            BASE64_ALPHABET
                .iter()
                .position(|candidate| *candidate == byte)
        };
        let a = decode(chunk[0]).ok_or_else(|| error("ORNA-EVAL-VALUE"))? as u8;
        let b = decode(chunk[1]).ok_or_else(|| error("ORNA-EVAL-VALUE"))? as u8;
        let first = (a << 2) | (b >> 4);
        output.push(first);
        if chunk[2] == b'=' {
            if !final_chunk || chunk[3] != b'=' || b & 0x0f != 0 {
                return Err(error("ORNA-EVAL-VALUE"));
            }
            continue;
        }
        let c = decode(chunk[2]).ok_or_else(|| error("ORNA-EVAL-VALUE"))? as u8;
        output.push((b << 4) | (c >> 2));
        if chunk[3] == b'=' {
            if !final_chunk || c & 0x03 != 0 {
                return Err(error("ORNA-EVAL-VALUE"));
            }
            continue;
        }
        let d = decode(chunk[3]).ok_or_else(|| error("ORNA-EVAL-VALUE"))? as u8;
        output.push((c << 6) | d);
    }
    Ok(output)
}

#[derive(Debug)]
enum JsonNode {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<JsonNode>),
    Object(Vec<(String, JsonNode)>),
}

struct RawJsonObject(Vec<(String, Box<RawValue>)>);

impl<'de> Deserialize<'de> for RawJsonObject {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct RawJsonObjectVisitor;
        impl<'de> Visitor<'de> for RawJsonObjectVisitor {
            type Value = RawJsonObject;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut names = BTreeSet::new();
                let mut values = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    if !names.insert(key.clone()) {
                        return Err(de::Error::custom("duplicate JSON object key"));
                    }
                    values.push((key, map.next_value::<Box<RawValue>>()?));
                }
                Ok(RawJsonObject(values))
            }
        }
        deserializer.deserialize_map(RawJsonObjectVisitor)
    }
}

fn parse_json_raw(raw: &str) -> Result<JsonNode, EvaluationError> {
    let raw = raw.trim();
    match raw.as_bytes().first().copied() {
        Some(b'{') => {
            let RawJsonObject(fields) =
                serde_json::from_str(raw).map_err(|_| error("ORNA-EVAL-VALUE"))?;
            fields
                .into_iter()
                .map(|(key, value)| Ok((key, parse_json_raw(value.get())?)))
                .collect::<Result<Vec<_>, EvaluationError>>()
                .map(JsonNode::Object)
        }
        Some(b'[') => {
            let values: Vec<Box<RawValue>> =
                serde_json::from_str(raw).map_err(|_| error("ORNA-EVAL-VALUE"))?;
            values
                .into_iter()
                .map(|value| parse_json_raw(value.get()))
                .collect::<Result<Vec<_>, _>>()
                .map(JsonNode::Array)
        }
        Some(b'"') => serde_json::from_str(raw)
            .map(JsonNode::String)
            .map_err(|_| error("ORNA-EVAL-VALUE")),
        Some(b't' | b'f') => serde_json::from_str(raw)
            .map(JsonNode::Bool)
            .map_err(|_| error("ORNA-EVAL-VALUE")),
        Some(b'n') => serde_json::from_str::<()>(raw)
            .map(|()| JsonNode::Null)
            .map_err(|_| error("ORNA-EVAL-VALUE")),
        Some(_) => Ok(JsonNode::Number(raw.to_owned())),
        None => Err(error("ORNA-EVAL-VALUE")),
    }
}

fn parse_json_node(input: &str) -> Result<JsonNode, EvaluationError> {
    let raw: Box<RawValue> = serde_json::from_str(input).map_err(|_| error("ORNA-EVAL-VALUE"))?;
    parse_json_raw(raw.get())
}

fn json_node_to_value(
    node: JsonNode,
    context: &mut Context<'_, '_>,
    depth: usize,
) -> Result<Value, EvaluationError> {
    context.depth(depth)?;
    context.step()?;
    match node {
        JsonNode::Null => Ok(Value::Null),
        JsonNode::Bool(value) => Ok(Value::Bool(value)),
        JsonNode::String(value) => context.string(value).map(Value::String),
        JsonNode::Number(number) => {
            if number.contains(['.', 'e', 'E']) {
                let (mantissa, exponent) = match number.find(['e', 'E']) {
                    Some(index) => (&number[..index], &number[index..]),
                    None => (number.as_str(), ""),
                };
                let mantissa = if mantissa.contains('.') {
                    mantissa.to_owned()
                } else {
                    format!("{mantissa}.0")
                };
                let normalized = format!("{mantissa}{exponent}");
                let (coefficient, exponent) = parse_decimal(&normalized)?;
                DecimalValue::new(coefficient, exponent).map(Value::Decimal)
            } else {
                let integer = BigInt::parse_bytes(number.as_bytes(), 10)
                    .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                context.integer(integer).map(Value::Int)
            }
        }
        JsonNode::Array(values) => {
            context.items(values.len())?;
            values
                .into_iter()
                .map(|value| json_node_to_value(value, context, depth + 1))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::List)
        }
        JsonNode::Object(entries) => {
            context.items(entries.len())?;
            let mut fields = BTreeMap::new();
            for (key, value) in entries {
                context.string(key.clone())?;
                fields.insert(key, json_node_to_value(value, context, depth + 1)?);
            }
            Ok(Value::Record(fields))
        }
    }
}

fn write_json_value(
    value: &Value,
    output: &mut String,
    context: &mut Context<'_, '_>,
    depth: usize,
) -> Result<(), EvaluationError> {
    context.depth(depth)?;
    context.step()?;
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Int(value) => write!(output, "{value}").map_err(|_| error("ORNA-EVAL-LIMIT"))?,
        Value::Decimal(value) => {
            output.push_str(&decimal_json_token(value)?);
        }
        Value::Float(bits) => {
            let number = f64::from_bits(*bits);
            if !number.is_finite() {
                return Err(error("ORNA-EVAL-VALUE"));
            }
            output.push_str(&number.to_string());
        }
        Value::String(value) => {
            output.push_str(&serde_json::to_string(value).map_err(|_| error("ORNA-EVAL-VALUE"))?)
        }
        Value::List(values) | Value::Tuple(values) => {
            context.items(values.len())?;
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_json_value(value, output, context, depth + 1)?;
            }
            output.push(']');
        }
        Value::Record(fields) => {
            context.items(fields.len())?;
            output.push('{');
            for (index, (key, value)) in fields.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(&serde_json::to_string(key).map_err(|_| error("ORNA-EVAL-VALUE"))?);
                output.push(':');
                write_json_value(value, output, context, depth + 1)?;
            }
            output.push('}');
        }
        Value::Option(None) => output.push_str("null"),
        Value::Option(Some(value)) => write_json_value(value, output, context, depth + 1)?,
        Value::Unit
        | Value::Money { .. }
        | Value::Blob(_)
        | Value::Date(_)
        | Value::Uuid(_)
        | Value::Reference(_)
        | Value::Instant { .. }
        | Value::Duration { .. }
        | Value::Period { .. }
        | Value::Error(_)
        | Value::Range { .. }
        | Value::Stream { .. }
        | Value::Relation(_)
        | Value::NominalRecord { .. }
        | Value::Enum { .. }
        | Value::Function { .. }
        | Value::Closure(_) => return Err(error("ORNA-EVAL-UNSUPPORTED")),
    }
    if output.len() > context.limits.max_string_bytes {
        return Err(error("ORNA-EVAL-LIMIT"));
    }
    Ok(())
}

fn decimal_json_token(value: &DecimalValue) -> Result<String, EvaluationError> {
    if value.coefficient.is_zero() {
        return Ok("0.0".to_owned());
    }
    let negative = value.coefficient.sign() == Sign::Minus;
    let digits = value.coefficient.abs().to_str_radix(10);
    let exponent = value
        .exponent10
        .to_i64()
        .filter(|exponent| exponent.unsigned_abs() <= DEFAULT_INTEGER_DIGITS as u64)
        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
    let mut token = String::new();
    if negative {
        token.push('-');
    }
    if exponent >= 0 {
        token.push_str(&digits);
        token.push_str(&"0".repeat(exponent as usize));
        token.push_str(".0");
    } else {
        let point = i64::try_from(digits.len()).map_err(|_| error("ORNA-EVAL-LIMIT"))? + exponent;
        if point > 0 {
            let point = usize::try_from(point).map_err(|_| error("ORNA-EVAL-LIMIT"))?;
            token.push_str(&digits[..point]);
            token.push('.');
            token.push_str(&digits[point..]);
        } else {
            token.push_str("0.");
            token.push_str(&"0".repeat(point.unsigned_abs() as usize));
            token.push_str(&digits);
        }
    }
    Ok(token)
}

fn encode_orna_value(value: &Value, depth: usize) -> Result<String, EvaluationError> {
    if depth > DEFAULT_DEPTH {
        return Err(error("ORNA-EVAL-LIMIT"));
    }
    Ok(match value {
        Value::Null => "null".to_owned(),
        Value::Unit => "()".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Int(value) => value.to_string(),
        Value::Decimal(value) => format!("{}e{}.decimal", value.coefficient, value.exponent10),
        Value::Float(bits) => {
            let value = f64::from_bits(*bits);
            if value.is_nan() {
                "Float.nan".to_owned()
            } else if value == f64::INFINITY {
                "Float.infinity".to_owned()
            } else if value == f64::NEG_INFINITY {
                "-Float.infinity".to_owned()
            } else {
                let text = format!("{value:.16e}");
                let (mantissa, exponent) = text
                    .split_once('e')
                    .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                let exponent = exponent
                    .parse::<i32>()
                    .map_err(|_| error("ORNA-EVAL-VALUE"))?;
                format!("{mantissa}e{exponent}f")
            }
        }
        Value::String(value) => encode_orna_string(value),
        Value::List(values) => format!(
            "[{}]",
            values
                .iter()
                .map(|value| encode_orna_value(value, depth + 1))
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        ),
        Value::Tuple(values) => {
            let mut parts = values
                .iter()
                .map(|value| encode_orna_value(value, depth + 1))
                .collect::<Result<Vec<_>, _>>()?;
            if parts.len() == 1 {
                parts[0].push(',');
            }
            format!("({})", parts.join(", "))
        }
        Value::Record(fields) => {
            if fields.is_empty() {
                "{}".to_owned()
            } else {
                let mut output = String::from("{\n");
                for (name, value) in fields {
                    if !is_identifier(name) {
                        return Err(error("ORNA-EVAL-UNSUPPORTED"));
                    }
                    output.push_str(&" ".repeat((depth + 1) * 2));
                    output.push_str(name);
                    output.push_str(": ");
                    output.push_str(&encode_orna_value(value, depth + 1)?);
                    output.push_str(",\n");
                }
                output.push_str(&" ".repeat(depth * 2));
                output.push('}');
                output
            }
        }
        Value::Option(None) => "null".to_owned(),
        Value::Option(Some(value)) => format!("Some({})", encode_orna_value(value, depth + 1)?),
        Value::Blob(value) => encode_orna_string(&encode_base64(value)),
        Value::Date(value) => format!("date{}", encode_orna_string(value)),
        Value::Instant { .. } | Value::Duration { .. } | Value::Money { .. } => {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        Value::Uuid(value) => encode_orna_string(&format_uuid_bytes(value)),
        Value::Reference(value) => encode_orna_string(&encode_base64(
            &value.encode().map_err(|_| error("ORNA-EVAL-VALUE"))?,
        )),
        Value::Period { .. }
        | Value::Error(_)
        | Value::Range { .. }
        | Value::Stream { .. }
        | Value::Relation(_)
        | Value::NominalRecord { .. }
        | Value::Enum { .. }
        | Value::Function { .. }
        | Value::Closure(_) => return Err(error("ORNA-EVAL-UNSUPPORTED")),
    })
}

fn encode_orna_string(value: &str) -> String {
    let mut output = String::from("\"");
    for scalar in value.chars() {
        match scalar {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '{' => output.push_str("\\u{7b}"),
            scalar if scalar.is_control() => {
                write!(output, "\\u{{{:x}}}", scalar as u32).expect("String writes succeed");
            }
            scalar => output.push(scalar),
        }
    }
    output.push('"');
    output
}

fn is_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_alphabetic())
        && chars.all(|scalar| scalar == '_' || scalar.is_alphanumeric())
}

fn is_orna_data_expression(expression: &Expr) -> bool {
    match expression {
        Expr::Literal { .. } => true,
        Expr::Name { .. } => false,
        Expr::Group { inner, .. } => is_orna_data_expression(inner),
        Expr::Unary { op, rhs, .. } => {
            matches!(op.as_str(), "+" | "-") && is_orna_data_expression(rhs)
        }
        Expr::List { elements, .. } | Expr::Tuple { elements, .. } => {
            elements.iter().all(is_orna_data_expression)
        }
        Expr::Record { fields, .. } => fields
            .iter()
            .all(|field| is_orna_data_expression(&field.value)),
        Expr::Field { base, .. } => matches!(
            base.as_ref(),
            Expr::Literal {
                kind: LiteralKind::Decimal | LiteralKind::Integer,
                ..
            }
        ),
        Expr::Call {
            callee, arguments, ..
        } if matches!(callee.as_ref(), Expr::Name { text, .. } if text == "Some")
            && arguments.len() == 1
            && arguments.iter().all(|argument| argument.name.is_none()) =>
        {
            arguments
                .iter()
                .all(|argument| is_orna_data_expression(&argument.value))
        }
        _ => false,
    }
}

fn format_uuid_bytes(bytes: &[u8; 16]) -> String {
    let mut output = String::with_capacity(36);
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            output.push('-');
        }
        write!(output, "{byte:02x}").expect("String writes succeed");
    }
    output
}

fn money_amount(value: &Value) -> Result<DecimalValue, EvaluationError> {
    match value {
        Value::Int(value) => DecimalValue::new(value.clone(), BigInt::zero()),
        Value::Decimal(value) => Ok(value.clone()),
        Value::Money { amount, .. } => Ok(amount.clone()),
        _ => Err(error("ORNA-EVAL-TYPE")),
    }
}

fn replace_money_amount(value: &Value, amount: DecimalValue) -> Result<Value, EvaluationError> {
    match value {
        Value::Money { currency, .. } => Ok(Value::Money {
            amount,
            currency: *currency,
        }),
        Value::Int(_) | Value::Decimal(_) => Ok(Value::Decimal(amount)),
        _ => Err(error("ORNA-EVAL-TYPE")),
    }
}

fn format_money_value(
    amount: &Value,
    currency_code: &str,
    minor_digits: usize,
    rounding: &str,
    locale: &str,
) -> Result<String, EvaluationError> {
    if currency_code.len() != 3
        || !currency_code
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_uppercase())
    {
        return Err(error("ORNA-EVAL-VALUE"));
    }
    let (grouping, decimal_separator, suffix_symbol) = match locale {
        "en" | "en-US" | "en-GB" | "ja-JP" => (",", ".", false),
        "fr-FR" => ("\u{202f}", ",", true),
        "de-DE" => (".", ",", true),
        _ => return Err(error("ORNA-EVAL-VALUE")),
    };
    let symbol = match currency_code {
        "GBP" => "£",
        "USD" => "$",
        "EUR" => "€",
        "JPY" => "¥",
        "CAD" => "CA$",
        "AUD" => "A$",
        "NZD" => "NZ$",
        "CHF" => "CHF",
        _ => currency_code,
    };
    let original = money_amount(amount)?;
    let rounded = quantize_decimal(&original, minor_digits, rounding)?;
    let units =
        decimal_scaled_integer(&rounded, minor_digits)?.ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
    let negative = units.sign() == Sign::Minus;
    let digits = units.abs().to_str_radix(10);
    let (whole, fraction) = if minor_digits == 0 {
        (digits, String::new())
    } else if digits.len() <= minor_digits {
        (
            "0".to_owned(),
            format!("{}{}", "0".repeat(minor_digits - digits.len()), digits),
        )
    } else {
        let split = digits.len() - minor_digits;
        (digits[..split].to_owned(), digits[split..].to_owned())
    };
    let mut grouped = String::with_capacity(whole.len() + whole.len() / 3);
    let mut first_group = whole.len() % 3;
    if first_group == 0 {
        first_group = 3;
    }
    grouped.push_str(&whole[..first_group]);
    for start in (first_group..whole.len()).step_by(3) {
        grouped.push_str(grouping);
        grouped.push_str(&whole[start..start + 3]);
    }
    let mut number = grouped;
    if minor_digits > 0 {
        number.push_str(decimal_separator);
        number.push_str(&fraction);
    }
    let sign = if negative { "-" } else { "" };
    if symbol == currency_code {
        if suffix_symbol {
            Ok(format!("{sign}{number}\u{00a0}{currency_code}"))
        } else {
            Ok(format!("{sign}{currency_code}\u{00a0}{number}"))
        }
    } else if suffix_symbol {
        Ok(format!("{sign}{number}\u{00a0}{symbol}"))
    } else {
        Ok(format!("{sign}{symbol}{number}"))
    }
}

fn quantize_decimal(
    value: &DecimalValue,
    scale: usize,
    rounding: &str,
) -> Result<DecimalValue, EvaluationError> {
    if scale > DEFAULT_INTEGER_DIGITS
        || !matches!(
            rounding,
            "half_even" | "half_up" | "toward_zero" | "away_from_zero" | "floor" | "ceil"
        )
    {
        return Err(error("ORNA-EVAL-VALUE"));
    }
    let shift = &value.exponent10 + BigInt::from(scale);
    if shift.sign() != Sign::Minus {
        let power = shift.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        if power > DEFAULT_INTEGER_DIGITS {
            return Err(error("ORNA-EVAL-LIMIT"));
        }
        return DecimalValue::new(
            &value.coefficient * BigInt::from(10u8).pow(power as u32),
            -BigInt::from(scale),
        );
    }
    let power = (-shift)
        .to_usize()
        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
    if power > DEFAULT_INTEGER_DIGITS {
        return Err(error("ORNA-EVAL-LIMIT"));
    }
    let divisor = BigInt::from(10u8).pow(power as u32);
    let (mut quotient, remainder) = value.coefficient.div_rem(&divisor);
    let negative = value.coefficient.sign() == Sign::Minus;
    if !remainder.is_zero() {
        let comparison = (remainder.abs() * 2u8).cmp(&divisor);
        let increment = match rounding {
            "toward_zero" => false,
            "away_from_zero" => true,
            "floor" => negative,
            "ceil" => !negative,
            "half_up" => comparison != std::cmp::Ordering::Less,
            "half_even" => {
                comparison == std::cmp::Ordering::Greater
                    || (comparison == std::cmp::Ordering::Equal
                        && (&quotient % 2u8).abs() == BigInt::from(1u8))
            }
            _ => return Err(error("ORNA-EVAL-VALUE")),
        };
        if increment {
            if negative {
                quotient -= 1;
            } else {
                quotient += 1;
            }
        }
    }
    DecimalValue::new(quotient, -BigInt::from(scale))
}

fn allocate_minor_units(
    amount: &Value,
    weights: &[BigInt],
    scale: usize,
    rounding: Option<&str>,
) -> Result<Vec<DecimalValue>, EvaluationError> {
    if weights.is_empty() || scale > DEFAULT_INTEGER_DIGITS {
        return Err(error("ORNA-EVAL-VALUE"));
    }
    let total_weight = weights
        .iter()
        .fold(BigInt::zero(), |total, weight| total + weight);
    if total_weight <= BigInt::zero() {
        return Err(error("ORNA-EVAL-VALUE"));
    }
    let original = money_amount(amount)?;
    let exact_units = decimal_scaled_integer(&original, scale)?;
    let amount_units = match (exact_units, rounding) {
        (Some(units), _) => units,
        (None, Some(rounding)) => {
            let rounded = quantize_decimal(&original, scale, rounding)?;
            decimal_scaled_integer(&rounded, scale)?.ok_or_else(|| error("ORNA-EVAL-VALUE"))?
        }
        (None, None) => return Err(error("ORNA-EVAL-VALUE")),
    };
    let negative = amount_units.sign() == Sign::Minus;
    let magnitude = amount_units.abs();
    let mut shares = Vec::with_capacity(weights.len());
    let mut remainders = Vec::with_capacity(weights.len());
    let mut assigned = BigInt::zero();
    for (index, weight) in weights.iter().enumerate() {
        let numerator = &magnitude * weight;
        let (share, remainder) = numerator.div_rem(&total_weight);
        assigned += &share;
        shares.push(share);
        remainders.push((index, remainder));
    }
    let leftover = (&magnitude - assigned)
        .to_usize()
        .filter(|count| *count <= weights.len())
        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
    remainders.sort_by(|(left_index, left), (right_index, right)| {
        right.cmp(left).then(left_index.cmp(right_index))
    });
    for (index, _) in remainders.into_iter().take(leftover) {
        shares[index] += 1;
    }
    shares
        .into_iter()
        .map(|share| {
            let share = if negative { -share } else { share };
            DecimalValue::new(share, -BigInt::from(scale))
        })
        .collect()
}

fn decimal_scaled_integer(
    value: &DecimalValue,
    scale: usize,
) -> Result<Option<BigInt>, EvaluationError> {
    let shift = &value.exponent10 + BigInt::from(scale);
    if shift.sign() != Sign::Minus {
        let power = shift.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        if power > DEFAULT_INTEGER_DIGITS {
            return Err(error("ORNA-EVAL-LIMIT"));
        }
        return Ok(Some(
            &value.coefficient * BigInt::from(10u8).pow(power as u32),
        ));
    }
    let power = (-shift)
        .to_usize()
        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
    if power > DEFAULT_INTEGER_DIGITS {
        return Err(error("ORNA-EVAL-LIMIT"));
    }
    let divisor = BigInt::from(10u8).pow(power as u32);
    let (units, remainder) = value.coefficient.div_rem(&divisor);
    Ok(remainder.is_zero().then_some(units))
}

fn pattern_names(pattern: &Pattern) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    collect_pattern_names(pattern, &mut names);
    names
}

fn collect_pattern_names(pattern: &Pattern, names: &mut BTreeSet<String>) {
    match pattern {
        Pattern::Name(name, _) => {
            names.insert(name.clone());
        }
        Pattern::Record { fields, .. } => {
            for (name, pattern, _) in fields {
                if let Some(pattern) = pattern {
                    collect_pattern_names(pattern, names);
                } else {
                    names.insert(name.clone());
                }
            }
        }
        Pattern::Tuple { elements, .. } | Pattern::List { elements, .. } => {
            for pattern in elements {
                collect_pattern_names(pattern, names);
            }
        }
        Pattern::Constructor {
            arguments, fields, ..
        } => {
            for pattern in arguments {
                collect_pattern_names(pattern, names);
            }
            for field in fields {
                if let Some(pattern) = &field.pattern {
                    collect_pattern_names(pattern, names);
                } else {
                    names.insert(field.name.clone());
                }
            }
        }
        Pattern::Wildcard(_) | Pattern::Literal { .. } => {}
    }
}

fn bind(
    pattern: &Pattern,
    value: Value,
    scope: &mut Scope,
    context: &Context,
    depth: usize,
) -> Result<bool, EvaluationError> {
    context.depth(depth)?;
    match pattern {
        Pattern::Name(name, _) => {
            if name == "_" {
                return Ok(true);
            }
            scope.0.insert(name.clone(), value);
            scope.1.remove(name);
            scope.2.remove(name);
            Ok(true)
        }
        Pattern::Wildcard(_) => Ok(true),
        Pattern::Literal {
            kind: LiteralKind::Null,
            ..
        } if matches!(value, Value::Option(None)) => Ok(true),
        Pattern::Literal { text, kind, .. } => Ok(context.literal(text, *kind)? == value),
        Pattern::Tuple { elements, .. } => match value {
            Value::Tuple(values) if values.len() == elements.len() => {
                context.items(values.len())?;
                for (pattern, value) in elements.iter().zip(values) {
                    if !bind(pattern, value, scope, context, depth + 1)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            _ => Ok(false),
        },
        Pattern::List { elements, .. } => match value {
            Value::List(values) if values.len() == elements.len() => {
                context.items(values.len())?;
                for (pattern, value) in elements.iter().zip(values) {
                    if !bind(pattern, value, scope, context, depth + 1)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            _ => Ok(false),
        },
        Pattern::Record { fields, .. } => match value {
            Value::Record(values) => {
                context.items(fields.len())?;
                for (name, pattern, _) in fields {
                    let Some(value) = values.get(name).cloned() else {
                        return Ok(false);
                    };
                    if let Some(pattern) = pattern {
                        if !bind(pattern, value, scope, context, depth + 1)? {
                            return Ok(false);
                        }
                    } else {
                        scope.0.insert(name.clone(), value);
                        scope.1.remove(name);
                    }
                }
                Ok(true)
            }
            _ => Ok(false),
        },
        Pattern::Constructor {
            path,
            arguments,
            fields,
            ..
        } => bind_constructor(path, arguments, fields, value, scope, context, depth),
    }
}
fn enum_variant_identity<'scope>(
    scope: &'scope Scope,
    owner: &str,
    variant: &str,
) -> Option<(&'scope Raw, &'scope Raw)> {
    if let Some(definition) = scope.3.get(owner) {
        return definition
            .variants
            .iter()
            .find(|candidate| candidate.name == variant)
            .map(|candidate| (&definition.type_id, &candidate.variant_id));
    }
    if owner.contains('.') {
        return None;
    }
    let mut found = None;
    for (name, definition) in &scope.3 {
        if name.rsplit('.').next() != Some(owner) {
            continue;
        }
        let Some(candidate) = definition
            .variants
            .iter()
            .find(|candidate| candidate.name == variant)
        else {
            continue;
        };
        if found.is_some() {
            return None;
        }
        found = Some((&definition.type_id, &candidate.variant_id));
    }
    found
}

fn bind_constructor(
    path: &[orna_syntax_v1::NameSegment],
    arguments: &[Pattern],
    fields: &[PatternField],
    value: Value,
    scope: &mut Scope,
    context: &Context,
    depth: usize,
) -> Result<bool, EvaluationError> {
    context.items(path.len())?;
    context.items(arguments.len())?;
    context.items(fields.len())?;
    if path.len() == 1 && path[0].text == "Some" {
        let [argument] = arguments else {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        };
        if !fields.is_empty() {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        return match value {
            Value::Option(Some(value)) => bind(argument, *value, scope, context, depth + 1),
            Value::Option(None) => Ok(false),
            _ => Ok(false),
        };
    }
    if path.len() < 2 || !arguments.is_empty() {
        return Err(error("ORNA-EVAL-UNSUPPORTED"));
    }
    let owner = path[..path.len() - 1]
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join(".");
    let owner = context.string(owner)?;
    let variant = context.string(path.last().expect("path length checked").text.clone())?;
    let qualified_name = context.string(format!("{owner}.{variant}"))?;
    let Some((expected_type, expected_variant)) = enum_variant_identity(scope, &owner, &variant)
        .or_else(|| {
            scope.0.get(&qualified_name).and_then(|value| match value {
                Value::Enum {
                    type_id,
                    variant_id,
                    ..
                } => Some((type_id, variant_id)),
                _ => None,
            })
        })
    else {
        return Err(error("ORNA-EVAL-UNSUPPORTED"));
    };
    let Value::Enum {
        type_id,
        variant_id,
        payload,
    } = value
    else {
        return Ok(false);
    };
    if type_id != *expected_type || variant_id != *expected_variant {
        return Ok(false);
    }
    match (fields, payload) {
        ([], None) => Ok(true),
        ([], Some(_)) | (_, None) => Ok(false),
        (_, Some(payload)) => bind_pattern_fields(fields, *payload, scope, context, depth + 1),
    }
}

fn bind_pattern_fields(
    patterns: &[PatternField],
    value: Value,
    scope: &mut Scope,
    context: &Context,
    depth: usize,
) -> Result<bool, EvaluationError> {
    context.depth(depth)?;
    for field in patterns {
        let field_value = match &value {
            Value::Record(fields) => fields.get(&field.name),
            Value::NominalRecord { fields, .. } => fields
                .iter()
                .find(|(key, _)| matches!(key, Raw::Text(name) if name == &field.name))
                .map(|(_, value)| value),
            _ => return Ok(false),
        };
        let Some(field_value) = field_value else {
            return Ok(false);
        };
        if let Some(pattern) = &field.pattern {
            if !bind(pattern, field_value.clone(), scope, context, depth + 1)? {
                return Ok(false);
            }
        } else {
            scope.0.insert(field.name.clone(), field_value.clone());
            scope.1.remove(&field.name);
        }
    }
    Ok(true)
}
fn named_arguments(
    function: &str,
    arguments: &[orna_syntax_v1::Argument],
    values: Vec<Value>,
    implicit: usize,
    collection: bool,
) -> Result<Vec<Value>, EvaluationError> {
    if !matches!(
        function,
        "mean"
            | "median"
            | "percentile"
            | "variance"
            | "standard_deviation"
            | "__mean"
            | "__median"
            | "__percentile"
            | "__variance"
            | "__standard_deviation"
            | "hash.sha256"
            | "hash.sha256_text"
            | "hash.to_hex"
            | "hash.from_hex"
            | "hash.domain_sha256"
            | "random.bytes"
            | "random.integer"
            | "random.choose"
            | "random.shuffle"
            | "base64.encode"
            | "base64.decode"
            | "money.format"
    ) && arguments.iter().all(|argument| argument.name.is_none())
    {
        return Ok(values);
    }
    let expected: &[&str] = match function {
        "increment" | "decrement" | "is_zero" => &["value"],
        "min" | "max" if collection => &["rows"],
        "min" | "max" => &["left", "right"],
        "clamp" => &["value", "min", "max"],
        "random.bytes" => &["count"],
        "random.integer" => &["lower_inclusive", "upper_exclusive"],
        "random.choose" | "random.shuffle" => &["values"],
        "trim" | "lower" | "upper" => &["value"],
        "split" => &["value", "separator"],
        "join" => &["values", "separator"],
        "starts_with" => &["value", "prefix"],
        "ends_with" => &["value", "suffix"],
        "contains" => &["value", "needle"],
        "replace" => &["value", "from", "to"],
        "normalise" => &["value", "form"],
        "offset_at" => &["instant", "zone"],
        "resolve_local" => match values.len() {
            3 => &["local", "zone", "ambiguous"],
            4 => &["local", "zone", "ambiguous", "gap"],
            _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
        },
        "duration.compact.format"
        | "duration.clock.format"
        | "duration.words.format"
        | "duration.iso.format" => match values.len() {
            1 => &["duration"],
            2 => &["duration", "context"],
            _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
        },
        "bit_or" | "bit_and" | "bit_xor" => &["left", "right"],
        "bit_not" => &["value"],
        "shift_left" | "shift_right" => &["value", "count"],
        "from_list" => &["values", "source_identity"],
        "from_provider" => &["source", "identity"],
        "for_each" => &["stream", "action"],
        "batch" => &["stream", "size"],
        "buffer" => &["stream", "capacity"],
        "merge" => &["streams"],
        "throttle" => &["stream", "duration", "drop_policy"],
        "debounce" => &["stream", "duration", "delivery_policy"],
        "retry" => &["stream", "policy"],
        "recover" => &["stream", "handler"],
        "parallel" | "race" => &["callbacks"],
        "timeout" => &["callback", "duration"],
        "chunk" => &["values", "size"],
        "recursive_cte" | "__recursive_cte" => &["anchor", "recursive_term"],
        "flatten" | "unique" | "pairs" => &["values"],
        "distinct" | "count" => &["rows"],
        "last" => &["rows"],
        "sum" | "__sum" | "__min" | "__max" | "__range" | "__mode" => &["rows"],
        "mean"
        | "median"
        | "variance"
        | "standard_deviation"
        | "__mean"
        | "__median"
        | "__variance"
        | "__standard_deviation" => match values.len() {
            1 => &["rows"],
            3 => &["rows", "scale", "rounding"],
            _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
        },
        "percentile" | "__percentile" => match values.len() {
            3 => &["rows", "p", "interpolation"],
            5 => &["rows", "p", "interpolation", "scale", "rounding"],
            _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
        },
        "__histogram" => &["rows", "bins", "include_final_upper"],
        "__rate" | "__derivative" | "__integrate" => &["points"],
        "hash.sha256" | "hash.sha256_text" | "base64.encode" | "base64.decode" => &["input"],
        "hash.to_hex" => &["digest"],
        "hash.from_hex" => &["value"],
        "hash.domain_sha256" => &["domain", "payload"],
        "money.format" => &[
            "amount",
            "currency_code",
            "minor_digits",
            "rounding",
            "locale",
        ],
        "first" => &["rows"],
        "one" => match values.len() {
            1 => &["rows"],
            2 => &["rows", "predicate"],
            _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
        },
        "every" | "exists" => &["rows", "predicate"],
        "union" => &["left", "right"],
        "take" | "drop" => &["rows", "count"],
        "map" | "flat_map" => &["rows", "transform"],
        "sort_by" => &["rows", "key"],
        "rank" => &["values", "key"],
        "__list_length" | "__numeric_sum" => &["values"],
        "__minimum" | "__maximum" => &["values"],
        "__list_concat" => &["left", "right"],
        "__stable_sort" | "__group_by" | "__rank" => &["values", "key"],
        "__asof_join" => &["left", "right", "time", "by"],
        "__bucket_by" => &["rows", "period", "zone"],
        "__strictly_ordered_time_series" => &["points"],
        "__window_rate"
        | "__window_rate_integrate"
        | "__window_derivative"
        | "__window_integrate" => &["points", "size", "step"],
        "filter" => &["rows", "predicate"],
        "partition" | "split_when" => &["values", "predicate"],
        "group_by" => &["values", "key"],
        "asof_join" => &["left", "right", "time", "by"],
        "zip" | "zip_exact" => &["left", "right"],
        "window" => match values.len() {
            2 => &["values", "size"],
            3 => &["values", "size", "step"],
            _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
        },
        _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
    };
    if values.len() != expected.len() {
        return Err(error("ORNA-EVAL-UNSUPPORTED"));
    }
    let mut ordered = vec![None; expected.len()];
    let mut positional = 0;
    let mut named_started = false;
    for (index, value) in values.into_iter().enumerate() {
        let name = index
            .checked_sub(implicit)
            .and_then(|index| arguments.get(index))
            .and_then(|argument| argument.name.as_deref());
        let position = if let Some(name) = name {
            named_started = true;
            expected
                .iter()
                .position(|expected| *expected == name)
                .or_else(|| match (function, name) {
                    // `std.math.clamp`'s pinned source uses `lower`/`upper`;
                    // the earlier evaluator surface also admitted `min`/`max`.
                    ("clamp", "lower" | "min") => Some(1),
                    ("clamp", "upper" | "max") => Some(2),
                    _ => None,
                })
                .or_else(|| {
                    // Older bounded evaluator fixtures used `values` for
                    // finite lists. Keep that alias at runtime while the
                    // pinned 1.0 source signature canonically names `rows`.
                    (name == "values" && expected.first() == Some(&"rows")).then_some(0)
                })
        } else if named_started {
            None
        } else {
            let position = Some(positional);
            positional += 1;
            position
        }
        .ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?;
        if ordered[position].replace(value).is_some() {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
    }
    ordered
        .into_iter()
        .map(|value| value.ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED")))
        .collect()
}
fn math_name(expression: &Expr) -> Option<&str> {
    standard_name(expression, "math")
}

fn bits_name(expression: &Expr) -> Option<&str> {
    standard_name(expression, "bits")
}

fn text_name(expression: &Expr) -> Option<&str> {
    standard_name(expression, "text")
}

fn collection_name(expression: &Expr) -> Option<&str> {
    standard_name(expression, "collection")
}
fn query_name(expression: &Expr) -> Option<&str> {
    standard_name(expression, "query")
}
fn portable_collection_name(expression: &Expr) -> Option<&str> {
    collection_name(expression).or_else(|| query_name(expression))
}

fn portable_collection_operation<'a>(
    expression: &'a Expr,
    resolved_function: Option<&'a str>,
) -> Option<&'a str> {
    resolved_function
        .and_then(standard_collection_function_operation)
        .or_else(|| portable_collection_name(expression))
}

fn standard_collection_function_operation(name: &str) -> Option<&str> {
    name.strip_prefix("std.collection.")
        .or_else(|| name.strip_prefix("std.query."))
}

fn portable_statistics_operation(
    expression: &Expr,
    resolved_function: Option<&str>,
    scope: &Scope,
) -> Option<&'static str> {
    let operation = resolved_function
        .and_then(statistics_function_operation)
        .or_else(|| {
            let root = function_root_name(expression)?;
            if scope.0.contains_key(root) && !scope.2.contains(root) {
                return None;
            }
            function_name(expression)
                .as_deref()
                .and_then(statistics_function_operation)
        });
    operation
}

fn statistics_function_operation(name: &str) -> Option<&'static str> {
    let operation = name.strip_prefix("std.stats.")?;
    Some(match operation {
        "mean" | "__mean" => "mean",
        "median" | "__median" => "median",
        "percentile" | "__percentile" => "percentile",
        "sum" | "__sum" => "sum",
        "min" | "__min" => "min",
        "max" | "__max" => "max",
        "range" | "__range" => "range",
        "mode" | "__mode" => "mode",
        "variance" | "__variance" => "variance",
        "standard_deviation" | "__standard_deviation" => "standard_deviation",
        "histogram" | "__histogram" => "histogram",
        "rate" | "__rate" => "rate",
        "derivative" | "__derivative" => "derivative",
        "integrate" | "__integrate" => "integrate",
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StandardBindingKind {
    Collection,
    Math,
    Text,
    Bits,
    Stream,
    Concurrent,
    Ui,
    Stats,
    Time,
    Hash,
    Random,
    Base64,
    Json,
    OrnaCodec,
    OvbCodec,
    Money,
}

#[derive(Clone, Copy)]
struct StandardBindingModule {
    prefix: &'static str,
    kind: StandardBindingKind,
    operations: &'static [&'static str],
}

#[derive(Clone, Copy)]
struct RegisteredStandardBinding {
    kind: StandardBindingKind,
    operation: &'static str,
}

// The registry connects pinned source identities to bounded evaluator
// intrinsics. System host effects use their separate generated registry.
const STANDARD_BINDING_MODULES: &[StandardBindingModule] = &[
    StandardBindingModule {
        prefix: "std.collection.",
        kind: StandardBindingKind::Collection,
        operations: &[
            "chunk",
            "flatten",
            "filter",
            "map",
            "flat_map",
            "sort_by",
            "rank",
            "take",
            "drop",
            "distinct",
            "unique",
            "union",
            "count",
            "first",
            "one",
            "sum",
            "min",
            "max",
            "every",
            "exists",
            "partition",
            "zip",
            "zip_exact",
            "group_by",
            "pairs",
            "window",
            "split_when",
            "bucket_by",
            "asof_join",
            "__list_length",
            "__list_concat",
            "__numeric_sum",
            "__stable_sort",
            "__minimum",
            "__maximum",
            "__group_by",
            "__rank",
            "__asof_join",
            "__bucket_by",
        ],
    },
    StandardBindingModule {
        prefix: "std.query.",
        kind: StandardBindingKind::Collection,
        operations: &[
            "chunk",
            "flatten",
            "filter",
            "map",
            "flat_map",
            "sort_by",
            "rank",
            "take",
            "drop",
            "distinct",
            "recursive_cte",
            "__recursive_cte",
            "unique",
            "union",
            "count",
            "first",
            "one",
            "sum",
            "min",
            "max",
            "every",
            "exists",
            "partition",
            "zip",
            "zip_exact",
            "group_by",
            "pairs",
            "window",
            "split_when",
            "bucket_by",
            "asof_join",
            "__list_length",
            "__list_concat",
            "__numeric_sum",
            "__stable_sort",
            "__minimum",
            "__maximum",
            "__group_by",
            "__rank",
            "__asof_join",
            "__bucket_by",
            "__strictly_ordered_time_series",
            "__window_rate",
            "__window_rate_integrate",
            "__window_derivative",
            "__window_integrate",
        ],
    },
    StandardBindingModule {
        prefix: "std.math.",
        kind: StandardBindingKind::Math,
        operations: &[],
    },
    StandardBindingModule {
        prefix: "std.text.",
        kind: StandardBindingKind::Text,
        operations: &[
            "trim",
            "split",
            "join",
            "starts_with",
            "ends_with",
            "contains",
            "replace",
            "normalise",
            "lower",
            "upper",
            "__trim",
            "__split",
            "__join",
            "__starts_with",
            "__ends_with",
            "__contains",
            "__replace",
            "__normalise",
            "__lower",
            "__upper",
        ],
    },
    StandardBindingModule {
        prefix: "std.bits.",
        kind: StandardBindingKind::Bits,
        operations: &[
            "bit_or",
            "bit_and",
            "bit_xor",
            "bit_not",
            "shift_left",
            "shift_right",
            "__bit_or",
            "__bit_and",
            "__bit_xor",
            "__bit_not",
            "__shift_left",
            "__shift_right",
        ],
    },
    StandardBindingModule {
        prefix: "std.stream.",
        kind: StandardBindingKind::Stream,
        operations: &[
            "from_provider",
            "from_list",
            "for_each",
            "batch",
            "buffer",
            "merge",
            "throttle",
            "debounce",
            "retry",
            "recover",
        ],
    },
    StandardBindingModule {
        prefix: "std.concurrent.",
        kind: StandardBindingKind::Concurrent,
        operations: &["parallel", "race", "timeout"],
    },
    StandardBindingModule {
        prefix: "std.ui.",
        kind: StandardBindingKind::Ui,
        operations: &["__node"],
    },
    StandardBindingModule {
        prefix: "std.stats.",
        kind: StandardBindingKind::Stats,
        operations: &[
            "__mean",
            "__median",
            "__percentile",
            "__sum",
            "__min",
            "__max",
            "__range",
            "__mode",
            "__variance",
            "__standard_deviation",
            "__histogram",
            "__rate",
            "__derivative",
            "__integrate",
        ],
    },
    StandardBindingModule {
        prefix: "std.time.",
        kind: StandardBindingKind::Time,
        operations: &[
            "offset_at",
            "resolve_local",
            "duration.compact.format",
            "duration.clock.format",
            "duration.words.format",
            "duration.iso.format",
        ],
    },
    StandardBindingModule {
        prefix: "std.hash.",
        kind: StandardBindingKind::Hash,
        operations: &[
            "sha256",
            "sha256_text",
            "domain_sha256",
            "to_hex",
            "from_hex",
        ],
    },
    StandardBindingModule {
        prefix: "std.random.",
        kind: StandardBindingKind::Random,
        operations: &[
            "bytes",
            "integer",
            "choose",
            "shuffle",
            "__bytes",
            "__integer",
            "__choose",
            "__shuffle",
        ],
    },
    StandardBindingModule {
        prefix: "std.encoding.base64.",
        kind: StandardBindingKind::Base64,
        operations: &["encode", "decode", "__encode", "__decode"],
    },
    StandardBindingModule {
        prefix: "std.encoding.json.",
        kind: StandardBindingKind::Json,
        operations: &["__encode", "__decode", "__decode_with_options"],
    },
    StandardBindingModule {
        prefix: "std.encoding.orna.",
        kind: StandardBindingKind::OrnaCodec,
        operations: &["__encode", "__decode"],
    },
    StandardBindingModule {
        prefix: "std.encoding.ovb.",
        kind: StandardBindingKind::OvbCodec,
        operations: &["__encode", "__decode"],
    },
    StandardBindingModule {
        prefix: "std.money.",
        kind: StandardBindingKind::Money,
        operations: &["__quantize", "__allocate", "__format"],
    },
];

fn registered_standard_binding(
    callee: &Expr,
    resolved_function: Option<&str>,
    scope: &Scope,
    allow_unresolved_qualified: bool,
    source_export_available: bool,
    captured_timezone_matches: bool,
) -> Option<RegisteredStandardBinding> {
    let spelling = function_name(callee);
    let name = resolved_function.or(spelling.as_deref())?;
    let (module, operation) = STANDARD_BINDING_MODULES.iter().find_map(|module| {
        let operation = name.strip_prefix(module.prefix)?;
        module
            .operations
            .iter()
            .copied()
            .find(|registered| *registered == operation)
            .map(|operation| (*module, operation))
    })?;
    if module.kind == StandardBindingKind::Time && !captured_timezone_matches {
        return None;
    }
    if resolved_function.is_none()
        && (scope.0.contains_key("std")
            || (!allow_unresolved_qualified && !source_export_available))
    {
        return None;
    }
    Some(RegisteredStandardBinding {
        kind: module.kind,
        operation,
    })
}

fn is_native_collection_binding(
    callee: &Expr,
    resolved_function: Option<&str>,
    scope: &Scope,
    allow_unresolved_qualified: bool,
    source_export_available: bool,
) -> bool {
    registered_standard_binding(
        callee,
        resolved_function,
        scope,
        allow_unresolved_qualified,
        source_export_available,
        true,
    )
    .is_some_and(|binding| binding.kind == StandardBindingKind::Collection)
}

fn captured_timezone_snapshot_matches(functions: &Functions) -> bool {
    let Some(version) = functions.get("std.time.timezone_data_version") else {
        return false;
    };
    if !version.parameters.is_empty() {
        return false;
    }
    matches!(
        &version.body,
        Expr::Literal {
            text,
            kind: LiteralKind::String,
            ..
        } if unescape_string(text).is_ok_and(|edition| edition == TIMEZONE_DATASET_VERSION)
    )
}

fn format_duration_context(context: Option<&Value>) -> Result<(), EvaluationError> {
    let (locale, zone) = match context {
        None => ("en", "UTC"),
        Some(Value::Record(fields)) => {
            let Some(Value::String(locale)) = fields.get("locale") else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            let Some(Value::String(zone)) = fields.get("time_zone") else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            (locale.as_str(), zone.as_str())
        }
        Some(_) => return Err(error("ORNA-EVAL-TYPE")),
    };
    // This first pinned profile implements English labels and validates the
    // context zone against the same immutable edition as local-time lookup.
    if !matches!(locale, "en" | "en-US" | "en-GB") {
        return Err(error("ORNA-EVAL-VALUE"));
    }
    resolve_time_zone(zone).map_err(|_| error("ORNA-EVAL-VALUE"))?;
    Ok(())
}

fn elapsed_total_nanoseconds(seconds: &BigInt, nanosecond: u32) -> BigInt {
    seconds * BigInt::from(1_000_000_000u32) + BigInt::from(nanosecond)
}

fn format_duration(name: &str, seconds: &BigInt, nanosecond: u32) -> String {
    const NANOS_PER_SECOND: u64 = 1_000_000_000;
    const NANOS_PER_MINUTE: u64 = 60 * NANOS_PER_SECOND;
    const NANOS_PER_HOUR: u64 = 60 * NANOS_PER_MINUTE;
    const NANOS_PER_DAY: u64 = 24 * NANOS_PER_HOUR;

    let mut total = seconds * BigInt::from(NANOS_PER_SECOND) + BigInt::from(nanosecond);
    let negative = total.sign() == Sign::Minus;
    if negative {
        total = -total;
    }
    let (days, remainder) = total.div_rem(&BigInt::from(NANOS_PER_DAY));
    let (hours, remainder) = remainder.div_rem(&BigInt::from(NANOS_PER_HOUR));
    let (minutes, remainder) = remainder.div_rem(&BigInt::from(NANOS_PER_MINUTE));
    let (whole_seconds, fraction) = remainder.div_rem(&BigInt::from(NANOS_PER_SECOND));
    let hours = hours.to_u32().expect("hour remainder is below one day");
    let minutes = minutes
        .to_u32()
        .expect("minute remainder is below one hour");
    let whole_seconds = whole_seconds
        .to_u32()
        .expect("second remainder is below one minute");
    let fraction = fraction.to_u32().expect("fraction is below one second");
    let fractional_seconds = if fraction == 0 {
        whole_seconds.to_string()
    } else {
        format!("{whole_seconds}.{fraction:09}")
            .trim_end_matches('0')
            .to_owned()
    };
    let sign = if negative { "-" } else { "" };

    match name {
        "duration.compact.format" => {
            let mut parts = Vec::new();
            if !days.is_zero() {
                parts.push(format!("{days}d"));
            }
            if hours > 0 {
                parts.push(format!("{hours}h"));
            }
            if minutes > 0 {
                parts.push(format!("{minutes}m"));
            }
            if whole_seconds > 0 || fraction > 0 || parts.is_empty() {
                parts.push(format!("{fractional_seconds}s"));
            }
            format!("{sign}{}", parts.join(" "))
        }
        "duration.clock.format" => {
            // This is elapsed time, not a wall clock: retain total hours
            // beyond 23, then enforce the output byte bound at the caller.
            let total_hours = days * BigInt::from(24u8) + BigInt::from(hours);
            let total_hours = total_hours.to_str_radix(10);
            let total_hours = if total_hours.len() < 2 {
                format!("{total_hours:0>2}")
            } else {
                total_hours
            };
            // Pad the integral seconds field before its decimal tail; padding
            // the combined fractional text leaves single-digit seconds short.
            let clock_seconds = if fraction == 0 {
                format!("{whole_seconds:02}")
            } else {
                format!("{whole_seconds:02}.{fraction:09}")
                    .trim_end_matches('0')
                    .to_owned()
            };
            format!("{sign}{total_hours}:{minutes:02}:{clock_seconds}")
        }
        "duration.words.format" => {
            let mut parts = Vec::new();
            if !days.is_zero() {
                parts.push(format!(
                    "{days} day{}",
                    if days == BigInt::from(1u8) { "" } else { "s" }
                ));
            }
            if hours > 0 {
                parts.push(format!("{hours} hour{}", if hours == 1 { "" } else { "s" }));
            }
            if minutes > 0 {
                parts.push(format!(
                    "{minutes} minute{}",
                    if minutes == 1 { "" } else { "s" }
                ));
            }
            if whole_seconds > 0 || fraction > 0 || parts.is_empty() {
                let singular = whole_seconds == 1 && fraction == 0;
                parts.push(format!(
                    "{fractional_seconds} second{}",
                    if singular { "" } else { "s" }
                ));
            }
            format!("{sign}{}", parts.join(", "))
        }
        "duration.iso.format" => {
            let mut output = String::from(sign);
            output.push('P');
            if !days.is_zero() {
                output.push_str(&days.to_str_radix(10));
                output.push('D');
            }
            if hours > 0 || minutes > 0 || whole_seconds > 0 || fraction > 0 || days.is_zero() {
                output.push('T');
                if hours > 0 {
                    output.push_str(&hours.to_string());
                    output.push('H');
                }
                if minutes > 0 {
                    output.push_str(&minutes.to_string());
                    output.push('M');
                }
                if whole_seconds > 0
                    || fraction > 0
                    || (days.is_zero() && hours == 0 && minutes == 0)
                {
                    output.push_str(&fractional_seconds);
                    output.push('S');
                }
            }
            output
        }
        _ => String::new(),
    }
}

fn parse_local_datetime(text: &str) -> Option<LocalDateTime> {
    // Keep this wire spelling intentionally narrow: ISO local civil seconds
    // plus an optional decimal fraction, without an offset or host locale.
    let (whole, fraction) = match text.split_once('.') {
        Some((whole, fraction)) => {
            if fraction.is_empty()
                || fraction.len() > 9
                || !fraction.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
            (whole, Some(fraction))
        }
        None => (text, None),
    };
    if whole.len() != 19
        || whole.as_bytes()[4] != b'-'
        || whole.as_bytes()[7] != b'-'
        || whole.as_bytes()[10] != b'T'
        || whole.as_bytes()[13] != b':'
        || whole.as_bytes()[16] != b':'
        || !whole
            .bytes()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7 | 10 | 13 | 16) || byte.is_ascii_digit())
    {
        return None;
    }
    let parse = |range: std::ops::Range<usize>| whole[range].parse::<u32>().ok();
    let year = i32::try_from(parse(0..4)?).ok()?;
    let month = u8::try_from(parse(5..7)?).ok()?;
    let day = u8::try_from(parse(8..10)?).ok()?;
    let hour = u8::try_from(parse(11..13)?).ok()?;
    let minute = u8::try_from(parse(14..16)?).ok()?;
    let second = u8::try_from(parse(17..19)?).ok()?;
    let nanosecond = fraction.map_or(Some(0), |fraction| {
        format!("{fraction:0<9}").parse::<u32>().ok()
    })?;
    LocalDateTime::new(year, month, day, hour, minute, second, nanosecond).ok()
}

fn stats_name(expression: &Expr) -> Option<&str> {
    standard_name(expression, "stats")
}

fn time_name(expression: &Expr) -> Option<&str> {
    standard_name(expression, "time")
}

fn root_stream_name(expression: &Expr) -> Option<&'static str> {
    match expression {
        Expr::Field { base, name, .. }
            if name == "from_list"
                && matches!(base.as_ref(), Expr::Name { text, .. } if text == "Stream") =>
        {
            Some("from_list")
        }
        Expr::Name { text, .. } if text == "for_each" => Some("for_each"),
        _ => None,
    }
}

fn root_collection_name(expression: &Expr) -> Option<&str> {
    let Expr::Name { text, .. } = expression else {
        return None;
    };
    matches!(
        text.as_str(),
        "count"
            | "first"
            | "last"
            | "one"
            | "min"
            | "max"
            | "sum"
            | "every"
            | "exists"
            | "map"
            | "project"
            | "flat_map"
            | "sort_by"
            | "bucket_by"
            | "rank"
            | "filter"
            | "distinct"
            | "__recursive_cte"
            | "union"
            | "pairs"
            | "take"
            | "drop"
            | "window"
    )
    .then_some(text.as_str())
}

fn is_relation_source(expression: &Expr) -> bool {
    matches!(
        expression,
        Expr::Field { base, name, .. }
            if name == "source"
                && matches!(base.as_ref(), Expr::ReplBinding { text, .. } if text == "$__orna_relation")
    )
}

fn relation_call_candidate(
    name: &str,
    arguments: &[orna_syntax_v1::Argument],
    scope: &Scope,
) -> bool {
    if matches!(name, "union" | "zip" | "zip_exact" | "asof_join") {
        return arguments
            .iter()
            .filter(|argument| {
                argument.name.as_deref().is_none_or(|name| {
                    matches!(name, "left" | "right") || (name == "rows" && arguments.len() == 1)
                })
            })
            .any(|argument| relation_expression_candidate(&argument.value, scope));
    }
    let relation_argument = arguments
        .iter()
        .find(|argument| argument.name.as_deref() == Some("rows"))
        .or_else(|| {
            arguments
                .iter()
                .find(|argument| argument.name.as_deref() == Some("values"))
        })
        .or_else(|| arguments.first());
    let Some(argument) = relation_argument else {
        return false;
    };
    relation_expression_candidate(&argument.value, scope)
}

fn relation_expression_candidate(expression: &Expr, scope: &Scope) -> bool {
    if system_relation_source(expression).is_some() {
        return true;
    }
    if let Expr::Name { text, .. } = expression {
        return matches!(scope.0.get(text), Some(Value::Relation(_)));
    }
    match expression {
        Expr::Call {
            callee, arguments, ..
        } if is_relation_source(callee) && arguments.len() == 1 => true,
        Expr::Binary { lhs, op, .. } if op == "|" => relation_expression_candidate(lhs, scope),
        Expr::Call {
            callee, arguments, ..
        } if root_collection_name(callee).is_some()
            || portable_collection_name(callee).is_some() =>
        {
            arguments
                .iter()
                .find(|argument| argument.name.as_deref() == Some("rows"))
                .or_else(|| {
                    arguments
                        .iter()
                        .find(|argument| argument.name.as_deref() == Some("values"))
                })
                .or_else(|| {
                    arguments
                        .iter()
                        .find(|argument| argument.name.as_deref() == Some("left"))
                })
                .or_else(|| arguments.first())
                .is_some_and(|argument| relation_expression_candidate(&argument.value, scope))
        }
        _ => false,
    }
}

fn relation_named_arguments(
    function: &str,
    arguments: &[orna_syntax_v1::Argument],
    values: Vec<Value>,
    implicit: usize,
) -> Result<Vec<Value>, EvaluationError> {
    let expected: &[&str] = match function {
        "filter" => &["rows", "predicate"],
        "map" | "project" | "flat_map" => &["rows", "transform"],
        "sort_by" => &["rows", "key"],
        "chunk" => &["values", "size"],
        "flatten" => &["values"],
        "partition" | "split_when" => &["values", "predicate"],
        "group_by" | "rank" => &["values", "key"],
        "zip" | "zip_exact" => &["left", "right"],
        "asof_join" => &["left", "right", "time", "by"],
        "bucket_by" => match values.len() {
            2 => &["rows", "period"],
            3 => &["rows", "period", "zone"],
            _ => return Err(error("ORNA-EVAL-ARGUMENT")),
        },
        "distinct" | "unique" | "pairs" => &["rows"],
        "union" => &["left", "right"],
        "take" | "drop" => &["rows", "count"],
        "window" => match values.len() {
            2 => &["rows", "size"],
            3 => &["rows", "size", "step"],
            _ => return Err(error("ORNA-EVAL-ARGUMENT")),
        },
        "one" => match values.len() {
            1 => &["rows"],
            2 => &["rows", "predicate"],
            _ => return Err(error("ORNA-EVAL-ARGUMENT")),
        },
        "every" | "exists" => &["rows", "predicate"],
        "count" | "first" | "last" | "sum" | "min" | "max" => &["rows"],
        _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
    };
    if values.len() > expected.len() {
        return Err(error("ORNA-EVAL-ARGUMENT"));
    }
    let mut ordered = vec![None; expected.len()];
    let mut positional = 0usize;
    let mut named_started = false;
    for (index, value) in values.into_iter().enumerate() {
        let name = index
            .checked_sub(implicit)
            .and_then(|index| arguments.get(index))
            .and_then(|argument| argument.name.as_deref());
        let position = if let Some(name) = name {
            named_started = true;
            expected.iter().position(|expected| *expected == name)
        } else if named_started {
            None
        } else {
            let position = Some(positional);
            positional += 1;
            position
        }
        .ok_or_else(|| error("ORNA-EVAL-ARGUMENT"))?;
        if implicit > 0 && position == 0 && name.is_some() {
            return Err(error("ORNA-EVAL-ARGUMENT"));
        }
        if ordered[position].replace(value).is_some() {
            return Err(error("ORNA-EVAL-ARGUMENT"));
        }
    }
    ordered
        .into_iter()
        .map(|value| value.ok_or_else(|| error("ORNA-EVAL-ARGUMENT")))
        .collect()
}
fn relation_statistics_arguments(
    function: &str,
    arguments: &[orna_syntax_v1::Argument],
    values: Vec<Value>,
    implicit: usize,
) -> Result<Vec<Value>, EvaluationError> {
    let expected: &[&str];
    let defaults: Vec<Option<Value>>;
    match function {
        "mean" | "median" | "variance" | "standard_deviation" => {
            expected = &["rows", "scale", "rounding"];
            defaults = vec![None, Some(Value::Null), Some(Value::Null)];
        }
        "percentile" => {
            expected = &["rows", "p", "interpolation", "scale", "rounding"];
            defaults = vec![None, None, None, Some(Value::Null), Some(Value::Null)];
        }
        "sum" | "min" | "max" | "range" | "mode" => {
            expected = &["rows"];
            defaults = vec![None];
        }
        "histogram" => {
            expected = &["rows", "bins", "include_final_upper"];
            defaults = vec![None, None, Some(Value::Bool(false))];
        }
        "rate" | "derivative" | "integrate" => {
            expected = &["points"];
            defaults = vec![None];
        }
        _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
    }
    if values.len() > expected.len() || defaults.len() != expected.len() {
        return Err(error("ORNA-EVAL-ARGUMENT"));
    }
    let mut ordered = vec![None; expected.len()];
    let mut positional = 0usize;
    let mut named_started = false;
    for (index, value) in values.into_iter().enumerate() {
        let name = index
            .checked_sub(implicit)
            .and_then(|index| arguments.get(index))
            .and_then(|argument| argument.name.as_deref());
        let position = if let Some(name) = name {
            named_started = true;
            expected.iter().position(|expected| *expected == name)
        } else if named_started {
            None
        } else {
            let position = Some(positional);
            positional += 1;
            position
        }
        .ok_or_else(|| error("ORNA-EVAL-ARGUMENT"))?;
        if implicit > 0 && position == 0 && name.is_some() {
            return Err(error("ORNA-EVAL-ARGUMENT"));
        }
        if ordered[position].replace(value).is_some() {
            return Err(error("ORNA-EVAL-ARGUMENT"));
        }
    }
    ordered
        .into_iter()
        .zip(defaults)
        .map(|(value, default)| {
            value
                .or_else(|| default.clone())
                .ok_or_else(|| error("ORNA-EVAL-ARGUMENT"))
        })
        .collect()
}

fn bucket_by_spec(period: &Value, zone: Option<&Value>) -> Result<BucketBySpec, EvaluationError> {
    let zone = match zone {
        Some(Value::String(value)) => Some(value.clone()),
        Some(_) => return Err(error("ORNA-EVAL-TYPE")),
        None => None,
    };
    match period {
        Value::Duration {
            seconds,
            nanosecond,
        } => Ok(BucketBySpec {
            period: BucketPeriod::Elapsed {
                seconds: seconds.clone(),
                nanosecond: *nanosecond,
            },
            zone,
        }),
        Value::Period { days } => {
            let days = days.to_u32().ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
            if days == 0 {
                return Err(error("ORNA-EVAL-VALUE"));
            }
            Ok(BucketBySpec {
                period: BucketPeriod::CalendarDays { days },
                zone,
            })
        }
        _ => Err(error("ORNA-EVAL-TYPE")),
    }
}
fn bucket_error(failure: RelationBucketError) -> EvaluationError {
    match failure {
        RelationBucketError::TypeMismatch => error("ORNA-EVAL-TYPE"),
        RelationBucketError::InvalidPeriod
        | RelationBucketError::MissingZone
        | RelationBucketError::OutOfOrder
        | RelationBucketError::BoundaryOverflow
        | RelationBucketError::AmbiguousBoundary
        | RelationBucketError::NonexistentBoundary
        | RelationBucketError::TimeZone(_) => error("ORNA-EVAL-VALUE"),
    }
}

fn relation_window_argument(value: &Value) -> Result<usize, EvaluationError> {
    let Value::Int(value) = value else {
        return Err(error("ORNA-EVAL-TYPE"));
    };
    if !value.is_positive() {
        return Err(error("ORNA-EVAL-VALUE"));
    }
    value.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))
}

fn standard_name<'a>(expression: &'a Expr, module: &str) -> Option<&'a str> {
    let Expr::Field { base, name, .. } = expression else {
        return None;
    };
    let Expr::Field {
        base,
        name: selected_module,
        ..
    } = base.as_ref()
    else {
        return None;
    };
    let Expr::Name { text, .. } = base.as_ref() else {
        return None;
    };
    (text == "std" && selected_module == module).then_some(name.as_str())
}
fn ui_input_type_name(name: &str) -> Option<&'static str> {
    Some(match name {
        "Text" | "Str" | "String" | "std.text" => "std.text",
        "Bool" | "BOOLEAN" | "BOOL" | "std.boolean" => "std.boolean",
        "Int" | "INTEGER" | "BIGINT" | "std.integer" => "std.integer",
        "Float" | "std.float" => "std.float",
        "Decimal" | "std.decimal" => "std.decimal",
        _ => return None,
    })
}

fn ui_value_type_name(value: &Value) -> String {
    match value {
        Value::Null => "std.null".into(),
        Value::Unit => "std.void".into(),
        Value::Bool(_) => "std.boolean".into(),
        Value::Int(_) => "std.integer".into(),
        Value::Decimal(_) => "std.decimal".into(),
        Value::Money { .. } => "std.money".into(),
        Value::Float(_) => "std.float".into(),
        Value::String(_) => "std.text".into(),
        Value::Blob(_) => "std.binary_large_object".into(),
        Value::Date(_) => "std.date".into(),
        Value::Uuid(_) => "std.uuid".into(),
        Value::Reference(_) => "std.reference".into(),
        Value::Instant { .. } => "std.timestamp".into(),
        Value::Duration { .. } => "std.duration".into(),
        Value::Period { .. } => "std.period".into(),
        Value::Error(_) => "std.error".into(),
        Value::Range { .. } => "std.range".into(),
        Value::List(_) => "std.list".into(),
        Value::Stream { .. } => "std.stream".into(),
        Value::Relation(_) => "std.relation".into(),
        Value::Tuple(_) => "std.tuple".into(),
        Value::Record(_) => "std.record".into(),
        Value::NominalRecord { .. } => "std.nominal_record".into(),
        Value::Enum { .. } => "std.enum".into(),
        Value::Option(Some(value)) => format!("std.option<{}>", ui_value_type_name(value)),
        Value::Option(None) => "std.option<unknown>".into(),
        Value::Function { .. } | Value::Closure(_) => "std.function".into(),
    }
}

fn ui_property_type_matches(type_name: &str, value: &Value) -> bool {
    let is_typed_option = || {
        type_name
            .strip_prefix("std.option<")
            .and_then(|inner| inner.strip_suffix('>'))
            .is_some_and(ui_type_name_is_well_formed)
    };
    match value {
        Value::Null => type_name == "std.null" || is_typed_option(),
        Value::Option(None) => is_typed_option(),
        _ => type_name == ui_value_type_name(value),
    }
}

fn ui_type_name_is_well_formed(type_name: &str) -> bool {
    if let Some(inner) = type_name
        .strip_prefix("std.option<")
        .and_then(|inner| inner.strip_suffix('>'))
    {
        return ui_type_name_is_well_formed(inner);
    }
    !type_name.is_empty()
        && type_name.split('.').all(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .is_some_and(|first| first == '_' || first.is_alphabetic())
                && chars.all(|char| char == '_' || char.is_alphanumeric())
        })
}

fn is_ui_presentation_node(
    value: &Value,
    depth: usize,
    max_depth: usize,
) -> Result<bool, EvaluationError> {
    if depth > max_depth {
        return Err(error("ORNA-EVAL-LIMIT"));
    }
    let Value::Record(fields) = value else {
        return Ok(false);
    };
    if !matches!(fields.get("kind"), Some(Value::String(kind)) if kind == "node") {
        return Ok(false);
    }
    let Some(Value::Record(contract)) = fields.get("contract") else {
        return Ok(false);
    };
    if !["id", "name", "version"]
        .into_iter()
        .all(|name| matches!(contract.get(name), Some(Value::String(value)) if !value.is_empty()))
    {
        return Ok(false);
    }
    let Some(Value::Record(properties)) = fields.get("properties") else {
        return Ok(false);
    };
    if !properties.iter().all(|(name, property)| {
        let Value::Record(typed) = property else {
            return false;
        };
        let (Some(Value::String(type_name)), Some(value)) = (typed.get("type"), typed.get("value"))
        else {
            return false;
        };
        !name.is_empty()
            && ui_property_type_matches(type_name, value)
            && value.clone().canonical().is_ok()
    }) {
        return Ok(false);
    }
    let Some(Value::Record(slots)) = fields.get("slots") else {
        return Ok(false);
    };
    let child_depth = depth
        .checked_add(1)
        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
    for slot in slots.values() {
        let Value::List(children) = slot else {
            return Ok(false);
        };
        for child in children {
            if !is_ui_presentation_node(child, child_depth, max_depth)? {
                return Ok(false);
            }
        }
    }
    let Some(Value::Record(actions)) = fields.get("actions") else {
        return Ok(false);
    };
    Ok(actions.values().all(is_ui_action_descriptor))
}

fn is_ui_action_descriptor(value: &Value) -> bool {
    let Value::Record(fields) = value else {
        return false;
    };
    let has_text =
        |name: &str| matches!(fields.get(name), Some(Value::String(value)) if !value.is_empty());
    has_text("action_id")
        && has_text("input_type")
        && fields
            .get("debug_kind")
            .is_none_or(|value| matches!(value, Value::Null | Value::String(_)))
}

fn function_name(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Name { text, .. } => Some(text.clone()),
        Expr::Field { base, name, .. } => Some(format!("{}.{}", function_name(base)?, name)),
        _ => None,
    }
}

fn codec_decode_operation(name: &str) -> Option<(&'static str, bool)> {
    match name {
        "std.encoding.json.decode" => Some(("json", false)),
        "std.encoding.json.decode_with_options" => Some(("json", true)),
        "std.encoding.orna.decode" => Some(("orna", false)),
        "std.encoding.ovb.decode" => Some(("ovb", false)),
        _ => None,
    }
}

#[derive(Clone, Debug)]
enum CodecTypeWitness {
    Named(String),
    List(Box<Self>),
    Tuple(Vec<Self>),
    Record(BTreeMap<String, Self>),
}

fn codec_type_witness(expression: &Expr) -> Option<CodecTypeWitness> {
    match expression {
        Expr::Group { inner, .. } => codec_type_witness(inner),
        Expr::List { elements, .. } if elements.len() == 1 => Some(CodecTypeWitness::List(
            Box::new(codec_type_witness(&elements[0])?),
        )),
        Expr::Tuple { elements, .. } => Some(CodecTypeWitness::Tuple(
            elements
                .iter()
                .map(codec_type_witness)
                .collect::<Option<Vec<_>>>()?,
        )),
        Expr::Record { fields, .. } => {
            let mut witness = BTreeMap::new();
            for field in fields {
                if !field.name.nfc().eq(field.name.chars())
                    || witness
                        .insert(field.name.clone(), codec_type_witness(&field.value)?)
                        .is_some()
                {
                    return None;
                }
            }
            Some(CodecTypeWitness::Record(witness))
        }
        _ => function_name(expression).map(CodecTypeWitness::Named),
    }
}

fn codec_type_matches(
    value: &Value,
    witness: &CodecTypeWitness,
    nominal_definitions: &NominalDefinitions,
) -> bool {
    match witness {
        CodecTypeWitness::Named(witness) => {
            if let Some(definition) = nominal_definitions.get(witness) {
                return matches!(
                    value,
                    Value::NominalRecord { type_id, .. } if *type_id == definition.type_id
                );
            }
            match witness.as_str() {
                "Str" | "Text" | "String" => matches!(value, Value::String(_)),
                "Bool" => matches!(value, Value::Bool(_)),
                "Int" | "Integer" => matches!(value, Value::Int(_)),
                "Decimal" => matches!(value, Value::Decimal(_)),
                "Float" => matches!(value, Value::Float(_)),
                "Blob" => matches!(value, Value::Blob(_)),
                "Unit" => matches!(value, Value::Unit),
                _ => false,
            }
        }
        CodecTypeWitness::List(element) => matches!(
            value,
            Value::List(values) if values.iter().all(|value| codec_type_matches(value, element, nominal_definitions))
        ),
        CodecTypeWitness::Tuple(elements) => match value {
            Value::Unit => elements.is_empty(),
            Value::Tuple(values) => {
                values.len() == elements.len()
                    && values.iter().zip(elements).all(|(value, element)| {
                        codec_type_matches(value, element, nominal_definitions)
                    })
            }
            _ => false,
        },
        CodecTypeWitness::Record(witness_fields) => match value {
            Value::Record(fields) if fields.len() == witness_fields.len() => {
                fields.iter().all(|(name, value)| {
                    witness_fields.get(name).is_some_and(|witness| {
                        codec_type_matches(value, witness, nominal_definitions)
                    })
                })
            }
            _ => false,
        },
    }
}

fn system_relation_source(expression: &Expr) -> Option<&'static str> {
    match function_name(expression)?.as_str() {
        "sys.Storage" => Some("sys.Storage"),
        "sys.MaintenanceJob" => Some("sys.MaintenanceJob"),
        _ => None,
    }
}

fn function_namespace(name: &str) -> Option<String> {
    name.rsplit_once('.')
        .map(|(namespace, _)| namespace.to_owned())
}

fn function_root_name(expression: &Expr) -> Option<&str> {
    match expression {
        Expr::Name { text, .. } => Some(text),
        Expr::Field { base, .. } => function_root_name(base),
        _ => None,
    }
}

fn is_static_effect_path(callee: &Expr, scope: &Scope) -> bool {
    matches!(callee, Expr::Field { base, .. }
        if matches!(base.as_ref(), Expr::ReplBinding { text, .. } if text == "$__orna_relation"))
        || matches!(callee, Expr::Field { .. })
            && function_root_name(callee)
                .is_some_and(|root| scope.4.contains(root) || !scope.0.contains_key(root))
}

fn one_like(value: &Value) -> Result<Value, EvaluationError> {
    match value {
        Value::Int(_) => Ok(Value::Int(1.into())),
        Value::Decimal(_) => DecimalValue::new(1.into(), 0.into()).map(Value::Decimal),
        Value::Float(_) => Ok(Value::Float(1.0f64.to_bits())),
        _ => Err(error("ORNA-EVAL-TYPE")),
    }
}
fn raw_option_value(value: Option<Box<Value>>) -> Result<Raw, EvaluationError> {
    let parts = match value {
        Some(value) => vec![Raw::Int(1.into()), value.raw()?],
        None => vec![Raw::Int(0.into())],
    };
    Ok(Raw::Tag(60013, Box::new(Raw::Array(parts))))
}
fn option_value(
    raw: &Raw,
    context: &mut Context,
    depth: usize,
) -> Result<Option<Box<Value>>, EvaluationError> {
    context.depth(depth)?;
    let Raw::Tag(60013, boxed) = raw else {
        return Err(error("ORNA-EVAL-VALUE"));
    };
    let Raw::Array(parts) = boxed.as_ref() else {
        return Err(error("ORNA-EVAL-VALUE"));
    };
    match parts.as_slice() {
        [Raw::Int(tag)] if tag.is_zero() => Ok(None),
        [Raw::Int(tag), value] if *tag == BigInt::from(1) => {
            Value::from_raw(value, context, depth + 1)
                .map(Box::new)
                .map(Some)
        }
        _ => Err(error("ORNA-EVAL-VALUE")),
    }
}
fn range_endpoint_kind(value: &Value) -> Option<&'static str> {
    match value {
        Value::Int(_) => Some("Int"),
        Value::Decimal(_) => Some("Decimal"),
        Value::Float(_) => Some("Float"),
        Value::Date(_) => Some("Date"),
        Value::Instant { .. } => Some("Instant"),
        Value::Duration { .. } => Some("Duration"),
        _ => None,
    }
}
fn validate_range_endpoints(
    lower: Option<&Value>,
    upper: Option<&Value>,
) -> Result<(), EvaluationError> {
    range_endpoint_kind_for_range(lower, upper).map(|_| ())
}
fn range_endpoint_kind_for_range(
    lower: Option<&Value>,
    upper: Option<&Value>,
) -> Result<&'static str, EvaluationError> {
    let endpoint = lower.or(upper).ok_or_else(|| error("ORNA-EVAL-TYPE"))?;
    let kind = range_endpoint_kind(endpoint).ok_or_else(|| error("ORNA-EVAL-TYPE"))?;
    if lower
        .into_iter()
        .chain(upper)
        .all(|endpoint| range_endpoint_kind(endpoint) == Some(kind))
    {
        Ok(kind)
    } else {
        Err(error("ORNA-EVAL-TYPE"))
    }
}
fn validate_comparable_range_endpoints(
    left_lower: Option<&Value>,
    left_upper: Option<&Value>,
    right_lower: Option<&Value>,
    right_upper: Option<&Value>,
) -> Result<(), EvaluationError> {
    let left = range_endpoint_kind_for_range(left_lower, left_upper)?;
    let right = range_endpoint_kind_for_range(right_lower, right_upper)?;
    if left == right {
        Ok(())
    } else {
        Err(error("ORNA-EVAL-TYPE"))
    }
}
fn compare(op: &str, ordering: std::cmp::Ordering) -> Result<Value, EvaluationError> {
    Ok(Value::Bool(match op {
        "<" => ordering.is_lt(),
        "<=" => ordering.is_le(),
        ">" => ordering.is_gt(),
        ">=" => ordering.is_ge(),
        _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
    }))
}
fn compare_values(left: &Value, right: &Value) -> Result<std::cmp::Ordering, EvaluationError> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => Ok(a.cmp(b)),
        (Value::Date(a), Value::Date(b)) => Ok(a.cmp(b)),
        (Value::Decimal(a), Value::Decimal(b)) => {
            let delta = &a.exponent10 - &b.exponent10;
            if delta
                .abs()
                .to_usize()
                .is_none_or(|value| value > DEFAULT_INTEGER_DIGITS)
            {
                return Err(error("ORNA-EVAL-LIMIT"));
            }
            let factor = BigInt::from(10u8).pow(delta.abs().to_u32().unwrap_or(0));
            Ok(if delta.sign() == Sign::Minus {
                a.coefficient.cmp(&(&b.coefficient * factor))
            } else {
                (&a.coefficient * factor).cmp(&b.coefficient)
            })
        }
        (
            Value::Money {
                amount: left_amount,
                currency: left_currency,
            },
            Value::Money {
                amount: right_amount,
                currency: right_currency,
            },
        ) => {
            if left_currency != right_currency {
                return Err(error("ORNA-EVAL-TYPE"));
            }
            compare_values(
                &Value::Decimal(left_amount.clone()),
                &Value::Decimal(right_amount.clone()),
            )
        }
        (Value::Float(a), Value::Float(b)) => f64::from_bits(*a)
            .partial_cmp(&f64::from_bits(*b))
            .ok_or_else(|| error("ORNA-EVAL-VALUE")),
        (
            Value::Instant {
                unix_seconds: left_seconds,
                nanosecond: left_nanosecond,
            },
            Value::Instant {
                unix_seconds: right_seconds,
                nanosecond: right_nanosecond,
            },
        ) => Ok(left_seconds
            .cmp(right_seconds)
            .then(left_nanosecond.cmp(right_nanosecond))),
        (
            Value::Duration {
                seconds: left_seconds,
                nanosecond: left_nanosecond,
            },
            Value::Duration {
                seconds: right_seconds,
                nanosecond: right_nanosecond,
            },
        ) => Ok(left_seconds
            .cmp(right_seconds)
            .then(left_nanosecond.cmp(right_nanosecond))),
        (
            Value::Range {
                lower: left_lower,
                upper: left_upper,
                upper_inclusive: left_inclusive,
            },
            Value::Range {
                lower: right_lower,
                upper: right_upper,
                upper_inclusive: right_inclusive,
            },
        ) => {
            validate_comparable_range_endpoints(
                left_lower.as_deref(),
                left_upper.as_deref(),
                right_lower.as_deref(),
                right_upper.as_deref(),
            )?;
            Ok(compare_range_lower(left_lower, right_lower)?
                .then(compare_range_upper(left_upper, right_upper)?)
                .then(left_inclusive.cmp(right_inclusive)))
        }
        _ => Err(error("ORNA-EVAL-TYPE")),
    }
}
fn range_membership_compare(
    left: &Value,
    right: &Value,
) -> Result<Option<std::cmp::Ordering>, EvaluationError> {
    match (left, right) {
        (Value::Float(left), Value::Float(right)) => {
            Ok(f64::from_bits(*left).partial_cmp(&f64::from_bits(*right)))
        }
        _ => compare_values(left, right).map(Some),
    }
}
fn compare_range_lower(
    left: &Option<Box<Value>>,
    right: &Option<Box<Value>>,
) -> Result<std::cmp::Ordering, EvaluationError> {
    match (left, right) {
        (None, None) => Ok(std::cmp::Ordering::Equal),
        (None, Some(_)) => Ok(std::cmp::Ordering::Less),
        (Some(_), None) => Ok(std::cmp::Ordering::Greater),
        (Some(left), Some(right)) => compare_range_endpoints(left, right),
    }
}
fn compare_range_upper(
    left: &Option<Box<Value>>,
    right: &Option<Box<Value>>,
) -> Result<std::cmp::Ordering, EvaluationError> {
    match (left, right) {
        (None, None) => Ok(std::cmp::Ordering::Equal),
        (None, Some(_)) => Ok(std::cmp::Ordering::Greater),
        (Some(_), None) => Ok(std::cmp::Ordering::Less),
        (Some(left), Some(right)) => compare_range_endpoints(left, right),
    }
}
fn compare_range_endpoints(
    left: &Value,
    right: &Value,
) -> Result<std::cmp::Ordering, EvaluationError> {
    match (left, right) {
        (Value::Float(left), Value::Float(right)) => Ok(float_total_cmp(*left, *right)),
        _ => compare_values(left, right),
    }
}
fn decimal_floor_fraction(value: &DecimalValue) -> Result<(usize, DecimalValue), EvaluationError> {
    if value.coefficient.is_negative() {
        return Err(error("ORNA-EVAL-VALUE"));
    }
    if !value.exponent10.is_negative() {
        let power = value
            .exponent10
            .to_usize()
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        if power > DEFAULT_INTEGER_DIGITS {
            return Err(error("ORNA-EVAL-LIMIT"));
        }
        let index = (&value.coefficient * BigInt::from(10u8).pow(power as u32))
            .to_usize()
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        return Ok((index, DecimalValue::new(BigInt::zero(), BigInt::zero())?));
    }
    let power = (-&value.exponent10)
        .to_usize()
        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
    if power > DEFAULT_INTEGER_DIGITS {
        return Err(error("ORNA-EVAL-LIMIT"));
    }
    let divisor = BigInt::from(10u8).pow(power as u32);
    let (index, remainder) = value.coefficient.div_rem(&divisor);
    Ok((
        index.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?,
        DecimalValue::new(remainder, value.exponent10.clone())?,
    ))
}

fn decimal_to_f64(value: &DecimalValue) -> Result<f64, EvaluationError> {
    let coefficient = value
        .coefficient
        .to_f64()
        .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
    let exponent = value
        .exponent10
        .to_i32()
        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
    let value = coefficient * 10f64.powi(exponent);
    value
        .is_finite()
        .then_some(value)
        .ok_or_else(|| error("ORNA-EVAL-VALUE"))
}

fn lawful_sort_key(value: &Value) -> Result<(), EvaluationError> {
    match value {
        Value::Bool(_)
        | Value::Int(_)
        | Value::Decimal(_)
        | Value::Float(_)
        | Value::Blob(_)
        | Value::String(_)
        | Value::Date(_)
        | Value::Instant { .. }
        | Value::Duration { .. } => Ok(()),
        Value::Range { .. } => Ok(()),
        Value::Tuple(values) => values.iter().try_for_each(lawful_sort_key),
        _ => Err(error("ORNA-EVAL-TYPE")),
    }
}
fn compare_sort_keys(left: &Value, right: &Value) -> Result<std::cmp::Ordering, EvaluationError> {
    match (left, right) {
        (Value::Bool(left), Value::Bool(right)) => Ok(left.cmp(right)),
        // Byte strings have a deterministic unsigned lexicographic order.
        // This lets source code sort by a complete OVB key encoding without
        // converting those bytes into a lossy textual representation.
        (Value::Blob(left), Value::Blob(right)) => Ok(left.cmp(right)),
        (Value::String(left), Value::String(right)) => Ok(left.cmp(right)),
        (Value::Date(left), Value::Date(right)) => Ok(left.cmp(right)),
        (
            Value::Instant {
                unix_seconds: left_seconds,
                nanosecond: left_nanosecond,
            },
            Value::Instant {
                unix_seconds: right_seconds,
                nanosecond: right_nanosecond,
            },
        ) => Ok(left_seconds
            .cmp(right_seconds)
            .then(left_nanosecond.cmp(right_nanosecond))),
        (
            Value::Duration {
                seconds: left_seconds,
                nanosecond: left_nanosecond,
            },
            Value::Duration {
                seconds: right_seconds,
                nanosecond: right_nanosecond,
            },
        ) => Ok(left_seconds
            .cmp(right_seconds)
            .then(left_nanosecond.cmp(right_nanosecond))),
        (Value::Float(left), Value::Float(right)) => Ok(float_total_cmp(*left, *right)),
        (Value::Tuple(left), Value::Tuple(right)) => {
            for (left, right) in left.iter().zip(right) {
                let ordering = compare_sort_keys(left, right)?;
                if ordering != std::cmp::Ordering::Equal {
                    return Ok(ordering);
                }
            }
            Ok(left.len().cmp(&right.len()))
        }
        (Value::Range { .. }, Value::Range { .. })
        | (Value::Int(_), Value::Int(_))
        | (Value::Decimal(_), Value::Decimal(_)) => compare_values(left, right),
        _ => Err(error("ORNA-EVAL-TYPE")),
    }
}
fn lawful_group_key(value: &Value) -> Result<(), EvaluationError> {
    match value {
        Value::Bool(_) | Value::Int(_) | Value::Decimal(_) | Value::String(_) => Ok(()),
        Value::Tuple(values) => values.iter().try_for_each(lawful_group_key),
        _ => Err(error("ORNA-EVAL-TYPE")),
    }
}
fn lawful_asof_time(value: &Value) -> Result<(), EvaluationError> {
    match value {
        Value::Int(_) | Value::Decimal(_) | Value::Date(_) | Value::Instant { .. } => Ok(()),
        _ => Err(error("ORNA-EVAL-TYPE")),
    }
}
fn compare_group_keys(left: &Value, right: &Value) -> Result<std::cmp::Ordering, EvaluationError> {
    match (left, right) {
        (Value::Bool(left), Value::Bool(right)) => Ok(left.cmp(right)),
        (Value::Int(left), Value::Int(right)) => Ok(left.cmp(right)),
        (Value::Decimal(left), Value::Decimal(right)) => compare_values(
            &Value::Decimal(left.clone()),
            &Value::Decimal(right.clone()),
        ),
        (Value::String(left), Value::String(right)) => Ok(left.cmp(right)),
        (Value::Tuple(left), Value::Tuple(right)) => {
            for (left, right) in left.iter().zip(right) {
                let ordering = compare_group_keys(left, right)?;
                if ordering != std::cmp::Ordering::Equal {
                    return Ok(ordering);
                }
            }
            Ok(left.len().cmp(&right.len()))
        }
        _ => Err(error("ORNA-EVAL-TYPE")),
    }
}
fn ordered(left: &Value, right: &Value, min: bool) -> Result<Value, EvaluationError> {
    let ordering = compare_values(left, right)?;
    if (ordering.is_le()) == min {
        Ok(left.clone())
    } else {
        Ok(right.clone())
    }
}
fn finite_float(value: f64) -> Result<Value, EvaluationError> {
    if value.is_finite() {
        Ok(Value::Float(value.to_bits()))
    } else {
        Err(error("ORNA-EVAL-VALUE"))
    }
}
fn parse_int(text: &str) -> Result<BigInt, EvaluationError> {
    let text = text.replace('_', "");
    if let Some(value) = text.strip_prefix("0x") {
        BigInt::parse_bytes(value.as_bytes(), 16).ok_or_else(|| error("ORNA-EVAL-VALUE"))
    } else if let Some(value) = text.strip_prefix("0b") {
        BigInt::parse_bytes(value.as_bytes(), 2).ok_or_else(|| error("ORNA-EVAL-VALUE"))
    } else {
        BigInt::parse_bytes(text.as_bytes(), 10).ok_or_else(|| error("ORNA-EVAL-VALUE"))
    }
}
fn parse_decimal(text: &str) -> Result<(BigInt, BigInt), EvaluationError> {
    let text = text.replace('_', "");
    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(index) => (
            &text[..index],
            text[index + 1..]
                .parse::<i64>()
                .map_err(|_| error("ORNA-EVAL-VALUE"))?,
        ),
        None => (text.as_str(), 0),
    };
    let (whole, fraction) = mantissa
        .split_once('.')
        .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
    let coefficient = BigInt::parse_bytes(format!("{whole}{fraction}").as_bytes(), 10)
        .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
    let fraction_digits = i64::try_from(fraction.len()).map_err(|_| error("ORNA-EVAL-LIMIT"))?;
    let exponent = exponent
        .checked_sub(fraction_digits)
        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
    Ok((coefficient, BigInt::from(exponent)))
}
fn unescape_string(text: &str) -> Result<String, EvaluationError> {
    if text.len() < 2 {
        return Err(error("ORNA-EVAL-VALUE"));
    }
    unescape_string_body(&text[1..text.len() - 1])
}
fn unescape_string_body(body: &str) -> Result<String, EvaluationError> {
    let mut output = String::new();
    let mut characters = body.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        match characters.next().ok_or_else(|| error("ORNA-EVAL-VALUE"))? {
            'n' => output.push('\n'),
            'r' => output.push('\r'),
            't' => output.push('\t'),
            '\\' => output.push('\\'),
            '"' => output.push('"'),
            'u' => {
                if characters.next() != Some('{') {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                let mut digits = String::new();
                loop {
                    let character = characters.next().ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                    if character == '}' {
                        break;
                    }
                    if !character.is_ascii_hexdigit() || digits.len() == 6 {
                        return Err(error("ORNA-EVAL-VALUE"));
                    }
                    digits.push(character);
                }
                if digits.is_empty() {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                let scalar =
                    u32::from_str_radix(&digits, 16).map_err(|_| error("ORNA-EVAL-VALUE"))?;
                output.push(char::from_u32(scalar).ok_or_else(|| error("ORNA-EVAL-VALUE"))?);
            }
            _ => return Err(error("ORNA-EVAL-VALUE")),
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn registry_covers_pinned_intrinsic_and_sys_host_stubs() {
        let scope = Scope(
            BTreeMap::new(),
            BTreeSet::new(),
            BTreeSet::new(),
            NominalDefinitions::new(),
            BTreeSet::new(),
        );
        let mut registered = 0;
        for (path, source) in orna_standard::reference_standard_sources_v1() {
            let parsed = orna_syntax_v1::parse_module(&source);
            assert!(parsed.is_ok(), "{path}: {:?}", parsed.diagnostics);
            let namespace = path
                .strip_prefix("std/")
                .and_then(|path| path.strip_suffix(".orna"))
                .expect("standard module path")
                .replace('/', ".");
            for item in parsed.value.items {
                let orna_syntax_v1::Declaration::Function { signature, body } = item.declaration
                else {
                    continue;
                };
                if !format!("{body:?}").contains("requires its") {
                    continue;
                }
                let qualified = format!("std.{namespace}.{}", signature.name);
                let call = orna_syntax_v1::parse_expression(&format!("{qualified}()"));
                assert!(call.is_ok(), "{qualified}: {:?}", call.diagnostics);
                let intrinsic = registered_standard_binding(
                    &call.value,
                    Some(&qualified),
                    &scope,
                    false,
                    true,
                    true,
                );
                let system_host = orna_sys_v1::system_host_operation_registry()
                    .operation(&qualified)
                    .is_some();
                if intrinsic.is_some() || system_host {
                    registered += 1;
                }
            }
        }
        assert!(
            registered >= 80,
            "expected the registered pinned host surface, got {registered}"
        );
    }

    fn evaluate_recovery(source: &str) -> CanonicalValue {
        evaluate_expression(source, &Environment::new(), Limits::default())
            .unwrap_or_else(|error| panic!("{source}: {}", error.code()))
    }

    fn integer(value: i64) -> CanonicalValue {
        CanonicalValue::new(Raw::Int(value.into())).expect("integer is canonical")
    }

    fn text(value: &str) -> CanonicalValue {
        CanonicalValue::new(Raw::Text(value.into())).expect("text is canonical")
    }

    fn test_context<'functions>(functions: &'functions Functions) -> Context<'functions, 'static> {
        Context {
            limits: Limits::default(),
            steps: 0,
            functions,
            aliases: None,
            session_functions: None,
            repl_bindings: false,
            restrict_function_names: false,
            reject_unhandled_field_calls: false,
            effects: None,
            namespace: None,
            transfer: None,
            cancellation: None,
        }
    }

    fn evaluate_relation_collection_fixture(
        source: &str,
        bindings: &[(&str, Vec<Value>)],
    ) -> Result<Value, EvaluationError> {
        fn materialize(
            context: &mut Context<'_, '_>,
            value: Value,
        ) -> Result<Value, EvaluationError> {
            match value {
                Value::Relation(plan) => {
                    Ok(Value::List(context.collect_relation_values(&plan, 0)?))
                }
                Value::Tuple(parts) => Ok(Value::Tuple(
                    parts
                        .into_iter()
                        .map(|part| materialize(context, part))
                        .collect::<Result<Vec<_>, _>>()?,
                )),
                Value::List(values) => Ok(Value::List(
                    values
                        .into_iter()
                        .map(|value| materialize(context, value))
                        .collect::<Result<Vec<_>, _>>()?,
                )),
                value => Ok(value),
            }
        }

        let parsed = parse_expression(source);
        assert!(parsed.is_ok(), "{source}: {:?}", parsed.diagnostics);
        let functions = Functions::new();
        let mut context = test_context(&functions);
        let mut scope = Scope(
            bindings
                .iter()
                .map(|(name, values)| {
                    (
                        (*name).to_owned(),
                        Value::Relation(RelationPlan::from_values(values.clone())),
                    )
                })
                .collect(),
            BTreeSet::new(),
            BTreeSet::new(),
            NominalDefinitions::new(),
            BTreeSet::new(),
        );
        let value = context.evaluate(&parsed.value, &mut scope, 0)?;
        materialize(&mut context, value)
    }

    #[test]
    fn relation_collection_group_and_partition_return_computed_relation_rows() {
        let ints = |values: &[i64]| {
            values
                .iter()
                .map(|value| Value::Int(BigInt::from(*value)))
                .collect::<Vec<_>>()
        };
        let grouped = evaluate_relation_collection_fixture(
            include_str!("../tests/fixtures/relation-group-by-result-0re2w.orna"),
            &[("rows", ints(&[31, 12, 22, 13, 33]))],
        )
        .expect("group_by computes a relation result");
        assert_eq!(
            grouped,
            Value::List(vec![
                Value::Tuple(vec![Value::Int(BigInt::from(1)), Value::List(ints(&[31]))]),
                Value::Tuple(vec![
                    Value::Int(BigInt::from(2)),
                    Value::List(ints(&[12, 22])),
                ]),
                Value::Tuple(vec![
                    Value::Int(BigInt::from(3)),
                    Value::List(ints(&[13, 33])),
                ]),
            ])
        );

        let partitioned = evaluate_relation_collection_fixture(
            include_str!("../tests/fixtures/relation-partition-result-0re2w.orna"),
            &[("rows", ints(&[3, 2, 4, 1]))],
        )
        .expect("partition computes both result relations");
        assert_eq!(
            partitioned,
            Value::Tuple(vec![Value::List(ints(&[2, 4])), Value::List(ints(&[3, 1]))])
        );
    }

    #[test]
    fn relation_collection_depth_operators_keep_real_values_and_order() {
        let ints = |values: &[i64]| {
            values
                .iter()
                .map(|value| Value::Int(BigInt::from(*value)))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            evaluate_relation_collection_fixture(
                include_str!("../tests/fixtures/relation-chunk-result-0re2w.orna"),
                &[("rows", ints(&[1, 2, 3]))],
            )
            .unwrap(),
            Value::List(vec![Value::List(ints(&[1, 2])), Value::List(ints(&[3]))])
        );
        assert_eq!(
            evaluate_relation_collection_fixture(
                include_str!("../tests/fixtures/relation-flatten-result-0re2w.orna"),
                &[(
                    "rows",
                    vec![Value::List(ints(&[1, 2])), Value::List(ints(&[3]))]
                )],
            )
            .unwrap(),
            Value::List(ints(&[1, 2, 3]))
        );
        let left = ints(&[1, 2]);
        let right = ["a", "b", "c"]
            .into_iter()
            .map(|value| Value::String(value.into()))
            .collect::<Vec<_>>();
        let zipped = Value::List(vec![
            Value::Tuple(vec![left[0].clone(), right[0].clone()]),
            Value::Tuple(vec![left[1].clone(), right[1].clone()]),
        ]);
        assert_eq!(
            evaluate_relation_collection_fixture(
                include_str!("../tests/fixtures/relation-zip-result-0re2w.orna"),
                &[("left", left.clone()), ("right", right.clone())],
            )
            .unwrap(),
            zipped
        );
        assert_eq!(
            evaluate_relation_collection_fixture(
                include_str!("../tests/fixtures/relation-zip-exact-result-0re2w.orna"),
                &[("left", left), ("right", right[..2].to_vec())],
            )
            .unwrap(),
            zipped
        );
        assert_eq!(
            evaluate_relation_collection_fixture(
                include_str!("../tests/fixtures/relation-unique-result-0re2w.orna"),
                &[("rows", ints(&[3, 1, 3, 2, 1]))],
            )
            .unwrap(),
            Value::List(ints(&[3, 1, 2]))
        );
        assert_eq!(
            evaluate_relation_collection_fixture(
                include_str!("../tests/fixtures/relation-split-when-result-0re2w.orna"),
                &[("rows", ints(&[1, 2, 3, 4]))],
            )
            .unwrap(),
            Value::List(vec![
                Value::List(ints(&[1])),
                Value::List(ints(&[2, 3])),
                Value::List(ints(&[4])),
            ])
        );
        assert_eq!(
            evaluate_relation_collection_fixture(
                include_str!("../tests/fixtures/relation-rank-result-0re2w.orna"),
                &[("rows", ints(&[3, 1, 2, 1]))],
            )
            .unwrap(),
            Value::List(vec![
                Value::Tuple(vec![
                    Value::Int(BigInt::from(1)),
                    Value::Int(BigInt::from(1))
                ]),
                Value::Tuple(vec![
                    Value::Int(BigInt::from(1)),
                    Value::Int(BigInt::from(1))
                ]),
                Value::Tuple(vec![
                    Value::Int(BigInt::from(2)),
                    Value::Int(BigInt::from(3))
                ]),
                Value::Tuple(vec![
                    Value::Int(BigInt::from(3)),
                    Value::Int(BigInt::from(4))
                ]),
            ])
        );
        assert_eq!(
            evaluate_relation_collection_fixture(
                include_str!("../tests/fixtures/relation-asof-result-0re2w.orna"),
                &[("left", ints(&[10])), ("right", ints(&[9, 11]))],
            )
            .unwrap(),
            Value::List(vec![Value::Tuple(vec![
                Value::Int(BigInt::from(10)),
                Value::Option(Some(Box::new(Value::Int(BigInt::from(9))))),
            ])])
        );
    }

    #[test]
    fn relation_query_aggregations_and_statistics_return_computed_values() {
        let ints = |values: &[i64]| {
            values
                .iter()
                .map(|value| Value::Int(BigInt::from(*value)))
                .collect::<Vec<_>>()
        };
        let result = evaluate_relation_collection_fixture(
            include_str!("../tests/fixtures/relation-query-statistics-6c10u.orna"),
            &[("rows", ints(&[0, 2]))],
        )
        .expect("query and statistics operators compute relation values");
        assert_eq!(
            result,
            Value::List(vec![
                Value::Int(BigInt::from(2)),
                Value::Int(BigInt::from(2)),
                Value::Option(Some(Box::new(Value::Int(BigInt::from(0))))),
                Value::Option(Some(Box::new(Value::Int(BigInt::from(2))))),
                Value::Int(BigInt::from(1)),
                Value::Int(BigInt::from(1)),
                Value::Decimal(DecimalValue::new(5.into(), (-1).into()).unwrap()),
                Value::Int(BigInt::from(2)),
                Value::Option(Some(Box::new(Value::Int(BigInt::from(0))))),
                Value::Option(Some(Box::new(Value::Int(BigInt::from(2))))),
                Value::Int(BigInt::from(2)),
                Value::List(ints(&[0, 2])),
                Value::Int(BigInt::from(1)),
                Value::Int(BigInt::from(1)),
                Value::List(ints(&[1, 1])),
            ])
        );

        let empty = evaluate_relation_collection_fixture(
            include_str!("../tests/fixtures/relation-statistics-empty-6c10u.orna"),
            &[("rows", Vec::new())],
        )
        .expect("empty aggregates use their documented identities");
        assert_eq!(
            empty,
            Value::List(vec![
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Int(BigInt::from(0)),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::List(Vec::new()),
                Value::Null,
                Value::Null,
                Value::List(ints(&[0])),
                Value::Int(BigInt::from(0)),
                Value::Int(BigInt::from(0)),
            ])
        );
    }

    #[test]
    fn relation_time_statistics_compute_rate_derivative_and_area() {
        fn instant(source: &str) -> Value {
            let parsed = parse_expression(source);
            assert!(parsed.is_ok(), "{source}: {:?}", parsed.diagnostics);
            let functions = Functions::new();
            let mut context = test_context(&functions);
            let mut scope = Scope(
                BTreeMap::new(),
                BTreeSet::new(),
                BTreeSet::new(),
                NominalDefinitions::new(),
                BTreeSet::new(),
            );
            context
                .evaluate(&parsed.value, &mut scope, 0)
                .unwrap_or_else(|error| panic!("{source}: {}", error.code()))
        }
        let first_time = instant("2024-01-01T00:00:00Z");
        let last_time = instant("2024-01-01T00:00:02Z");
        let points = vec![
            Value::Tuple(vec![first_time, Value::Int(BigInt::from(1))]),
            Value::Tuple(vec![last_time.clone(), Value::Int(BigInt::from(3))]),
        ];
        let result = evaluate_relation_collection_fixture(
            include_str!("../tests/fixtures/relation-statistics-time-series-6c10u.orna"),
            &[("points", points)],
        )
        .expect("time-series statistics consume ordered relation points");
        assert_eq!(
            result,
            Value::List(vec![
                Value::Int(BigInt::from(1)),
                Value::List(vec![Value::Tuple(vec![
                    last_time,
                    Value::Int(BigInt::from(1)),
                ])]),
                Value::Int(BigInt::from(4)),
            ])
        );
    }

    #[test]
    fn function_defaults_bind_in_declaration_order_and_only_when_omitted() {
        let parsed = orna_syntax_v1::parse_module(
            "fn run(seed: Int = 3, doubled: Int = seed * 2) = doubled;",
        );
        assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
        let orna_syntax_v1::Declaration::Function { signature, body } =
            &parsed.value.items[0].declaration
        else {
            panic!("function expected");
        };
        let functions = Functions::from([(
            signature.name.clone(),
            PureFunction {
                parameters: signature.parameters.clone(),
                body: body.clone(),
                environment: Environment::new(),
            },
        )]);

        assert_eq!(
            invoke_named("run", &functions, &Environment::new(), Limits::default()).unwrap(),
            integer(6)
        );
        assert_eq!(
            invoke_named(
                "run",
                &functions,
                &Environment::from([(String::from("seed"), integer(4))]),
                Limits::default(),
            )
            .unwrap(),
            integer(8)
        );
        assert_eq!(
            invoke_named(
                "run",
                &functions,
                &Environment::from([
                    (String::from("seed"), integer(4)),
                    (String::from("doubled"), integer(99)),
                ]),
                Limits::default(),
            )
            .unwrap(),
            integer(99)
        );
    }

    #[test]
    fn nominal_decode_rejects_duplicate_selected_and_unselected_field_keys() {
        let functions = Functions::new();
        for fields in [
            vec![
                Raw::Array(vec![object_id_raw([2; 16]), Raw::Int(1.into())]),
                Raw::Array(vec![object_id_raw([2; 16]), Raw::Int(2.into())]),
            ],
            vec![
                Raw::Array(vec![object_id_raw([2; 16]), Raw::Int(1.into())]),
                Raw::Array(vec![object_id_raw([3; 16]), Raw::Int(2.into())]),
                Raw::Array(vec![object_id_raw([3; 16]), Raw::Int(3.into())]),
            ],
        ] {
            let raw = Raw::Tag(
                60009,
                Box::new(Raw::Array(vec![object_id_raw([1; 16]), Raw::Array(fields)])),
            );
            let mut context = test_context(&functions);
            assert_eq!(
                Value::from_raw(&raw, &mut context, 0)
                    .expect_err("duplicate nominal field keys must fail closed")
                    .code(),
                "ORNA-EVAL-VALUE"
            );
        }
    }

    #[test]
    fn nominal_selection_rejects_duplicate_keys_even_when_unselected() {
        let functions = Functions::new();
        let mut context = test_context(&functions);
        let definition = NominalDefinition::new(
            [1; 16],
            None,
            vec![
                NominalField::public([2; 16], "value"),
                NominalField::public([3; 16], "other"),
            ],
        );
        let scope = Scope(
            BTreeMap::new(),
            BTreeSet::new(),
            BTreeSet::new(),
            NominalDefinitions::from([("Thing".into(), definition)]),
            BTreeSet::new(),
        );
        let fields = vec![
            (object_id_raw([2; 16]), Value::Int(1.into())),
            (object_id_raw([3; 16]), Value::Int(2.into())),
            (object_id_raw([3; 16]), Value::Int(3.into())),
        ];

        assert_eq!(
            context
                .nominal_field(&object_id_raw([1; 16]), &fields, "value", &scope)
                .expect_err("all nominal payload keys must be unique")
                .code(),
            "ORNA-EVAL-VALUE"
        );
    }

    struct FixedNow {
        value: CanonicalValue,
        calls: usize,
    }

    impl EffectHandler for FixedNow {
        fn handle(
            &mut self,
            callee: &Expr,
            arguments: &[CanonicalValue],
        ) -> Result<Option<CanonicalValue>, EvaluationError> {
            if matches!(callee, Expr::Name { text, .. } if text == "now") {
                assert!(arguments.is_empty());
                self.calls += 1;
                Ok(Some(self.value.clone()))
            } else {
                Ok(None)
            }
        }
    }

    #[test]
    fn bare_now_is_activation_effectful_and_user_function_shadowing_wins() {
        let instant = CanonicalValue::new(Raw::Tag(
            60002,
            Box::new(Raw::Array(vec![
                Raw::Int(1_700_000_000.into()),
                Raw::Int(7.into()),
            ])),
        ))
        .expect("canonical instant");
        let functions = {
            let parsed = orna_syntax_v1::parse_module("fn main() = [now(), now()];");
            assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
            let orna_syntax_v1::Declaration::Function { signature, body } =
                &parsed.value.items[0].declaration
            else {
                panic!("function expected")
            };
            Functions::from([(
                signature.name.clone(),
                PureFunction {
                    parameters: signature.parameters.clone(),
                    body: body.clone(),
                    environment: Environment::new(),
                },
            )])
        };
        let mut effects = FixedNow {
            value: instant.clone(),
            calls: 0,
        };
        let repeated = invoke_named_with_effects(
            "main",
            &functions,
            &Environment::new(),
            Limits::default(),
            &mut effects,
        )
        .expect("now effect result");
        assert_eq!(
            repeated,
            CanonicalValue::new(Raw::Array(vec![
                instant.raw().clone(),
                instant.raw().clone()
            ]))
            .expect("canonical repeated result")
        );
        assert_eq!(effects.calls, 2);

        let parsed = orna_syntax_v1::parse_module("fn now() = 7; fn main() = now();");
        assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
        let functions = parsed
            .value
            .items
            .into_iter()
            .map(|item| {
                let orna_syntax_v1::Declaration::Function { signature, body } = item.declaration
                else {
                    panic!("function expected")
                };
                (
                    signature.name,
                    PureFunction {
                        parameters: signature.parameters,
                        body,
                        environment: Environment::new(),
                    },
                )
            })
            .collect();
        let mut effects = FixedNow {
            value: instant,
            calls: 0,
        };
        assert_eq!(
            invoke_named_with_effects(
                "main",
                &functions,
                &Environment::new(),
                Limits::default(),
                &mut effects,
            )
            .expect("shadowed now result"),
            CanonicalValue::new(Raw::Int(7.into())).expect("canonical integer")
        );
        assert_eq!(effects.calls, 0);
    }

    #[test]
    fn recovery_pipelines_handle_ordinary_failures_and_skip_successes() {
        assert_eq!(
            evaluate_recovery("(1 / 0) |? (failure => if failure == failure { 42 } else { 0 })"),
            integer(42),
        );
        assert_eq!(evaluate_recovery("7 |? missing"), integer(7),);
        assert_eq!(
            evaluate_recovery("((1 / 0) |? (failure => 41)) | std.math.increment"),
            integer(42),
        );
    }

    #[test]
    fn error_intrinsic_builds_tagged_error_and_rejects_successful_escape() {
        let failure = evaluate_expression(
            r#"error(code: "message.invalid", message: "invalid message")"#,
            &Environment::new(),
            Limits::default(),
        )
        .unwrap_err();
        let canonical = failure.canonical_error().expect("canonical Error identity");
        assert_eq!(canonical.code(), "message.invalid");
        assert_eq!(canonical.message(), "invalid message");
        assert!(canonical.causes().is_empty());
        assert!(canonical.safe_details().is_empty());
        assert!(matches!(canonical.value().raw(), Raw::Tag(60016, _)));
    }

    #[test]
    fn deliberate_error_does_not_leak_source_code_into_diagnostics() {
        let failure = evaluate_expression(
            r#"error(code: "source\ncontrolled", message: "private message")"#,
            &Environment::new(),
            Limits::default(),
        )
        .unwrap_err();

        assert_eq!(
            failure.canonical_error().expect("canonical Error").code(),
            "source\ncontrolled"
        );
        assert_eq!(failure.diagnostic().code(), "ORNA-EVAL-ERROR");
        assert_eq!(failure.code(), "ORNA-EVAL-ERROR");
        assert_eq!(failure.to_string(), "ORNA-EVAL-ERROR");
        assert_eq!(failure.diagnostic().message(), "<redacted>");
    }

    #[test]
    fn fail_reemits_deliberate_error_byte_identically() {
        let source = r#"error(code: "inner", message: "inner")"#;
        let original = evaluate_expression(source, &Environment::new(), Limits::default())
            .unwrap_err()
            .canonical_error()
            .expect("canonical Error")
            .encode()
            .expect("canonical encoding");
        let failure = evaluate_expression(
            r#"fail(error(code: "inner", message: "inner")) |? (failure => fail(error(
                code: "outer",
                message: "outer",
                cause: failure,
            )))"#,
            &Environment::new(),
            Limits::default(),
        )
        .unwrap_err();
        let canonical = failure.canonical_error().expect("canonical Error identity");
        assert_eq!(canonical.code(), "outer");
        assert_eq!(canonical.causes().len(), 1);
        assert_eq!(canonical.causes()[0].code(), "inner");

        let reemitted = evaluate_expression(
            r#"fail(error(code: "inner", message: "inner")) |? (failure => fail(failure))"#,
            &Environment::new(),
            Limits::default(),
        )
        .unwrap_err()
        .canonical_error()
        .expect("canonical Error")
        .encode()
        .expect("canonical encoding");
        assert_eq!(reemitted, original);
    }

    #[test]
    fn ordinary_failure_reemits_its_original_identity() {
        let original =
            evaluate_expression("1 / 0", &Environment::new(), Limits::default()).unwrap_err();
        let reemitted = evaluate_expression(
            "(1 / 0) |? (failure => fail(failure))",
            &Environment::new(),
            Limits::default(),
        )
        .unwrap_err();

        assert_eq!(original.code(), "ORNA-EVAL-DIVIDE-BY-ZERO");
        assert_eq!(reemitted.code(), original.code());
        assert!(original.canonical_error().is_none());
        assert_eq!(
            reemitted
                .canonical
                .as_ref()
                .expect("retained ordinary identity")
                .encode()
                .expect("canonical encoding"),
            original
                .canonical
                .as_ref()
                .expect("retained ordinary identity")
                .encode()
                .expect("canonical encoding"),
        );
    }

    #[test]
    fn recovery_preserves_ordinary_failure_identity_inside_lists_and_records() {
        let original =
            evaluate_expression("1 / 0", &Environment::new(), Limits::default()).unwrap_err();
        let original_bytes = original
            .canonical
            .as_ref()
            .expect("ordinary identity")
            .encode()
            .expect("canonical encoding");

        for source in [
            "(1 / 0) |? (failure => [failure])",
            "(1 / 0) |? (failure => { error: failure })",
        ] {
            let recovered =
                evaluate_expression(source, &Environment::new(), Limits::default()).unwrap_err();
            assert_eq!(recovered.code(), original.code(), "{source}");
            assert_eq!(
                recovered
                    .canonical
                    .as_ref()
                    .expect("nested ordinary identity")
                    .encode()
                    .expect("canonical encoding"),
                original_bytes,
                "{source}",
            );
        }
    }

    #[test]
    fn recovery_preserves_deliberate_error_bytes_inside_lists_and_records() {
        let error = r#"error(code: "nested.code", message: "nested message")"#;
        let expected = evaluate_expression(error, &Environment::new(), Limits::default())
            .unwrap_err()
            .canonical_error()
            .expect("deliberate identity")
            .encode()
            .expect("canonical encoding");

        for source in [
            r#"fail(error(code: "nested.code", message: "nested message")) |? (failure => [failure])"#,
            r#"fail(error(code: "nested.code", message: "nested message")) |? (failure => { error: failure })"#,
        ] {
            let recovered =
                evaluate_expression(source, &Environment::new(), Limits::default()).unwrap_err();
            assert_eq!(
                recovered
                    .canonical_error()
                    .expect("nested deliberate identity")
                    .encode()
                    .expect("canonical encoding"),
                expected,
                "{source}",
            );
        }
    }

    #[test]
    fn recovery_reemits_the_selected_cause_byte_identically() {
        let selected = r#"error(code: "selected", message: "selected")"#;
        let expected = evaluate_expression(selected, &Environment::new(), Limits::default())
            .unwrap_err()
            .canonical_error()
            .expect("selected cause identity")
            .encode()
            .expect("canonical encoding");
        let recovered = evaluate_expression(
            r#"fail(error(code: "outer", message: "outer", cause: error(code: "selected", message: "selected"))) |? (failure => fail(failure.causes[0]))"#,
            &Environment::new(),
            Limits::default(),
        )
        .unwrap_err();

        assert_eq!(
            recovered
                .canonical_error()
                .expect("selected cause re-emission")
                .encode()
                .expect("canonical encoding"),
            expected,
        );
    }

    #[test]
    fn nested_errors_propagate_through_every_canonical_container() {
        let ordinary =
            evaluate_expression("1 / 0", &Environment::new(), Limits::default()).unwrap_err();
        let deliberate = EvaluationError::from_canonical(
            CanonicalErrorValue::new("nested.code", "nested message", [], BTreeMap::new())
                .expect("deliberate error is canonical"),
        );
        let containers = |failure: EvaluationError| {
            vec![
                Value::List(vec![Value::Error(failure.clone())]),
                Value::Tuple(vec![Value::Error(failure.clone())]),
                Value::Record(BTreeMap::from([(
                    "error".into(),
                    Value::Error(failure.clone()),
                )])),
                Value::NominalRecord {
                    type_id: Raw::Text("Example".into()),
                    fields: vec![(Raw::Text("error".into()), Value::Error(failure.clone()))],
                },
                Value::Enum {
                    type_id: Raw::Text("Example".into()),
                    variant_id: Raw::Text("Failure".into()),
                    payload: Some(Box::new(Value::Error(failure.clone()))),
                },
                Value::Option(Some(Box::new(Value::Error(failure.clone())))),
                Value::Range {
                    lower: Some(Box::new(Value::Error(failure))),
                    upper: None,
                    upper_inclusive: false,
                },
            ]
        };

        for value in containers(ordinary.clone()) {
            assert_eq!(value.canonical().unwrap_err(), ordinary);
        }

        let expected = deliberate
            .canonical_error()
            .expect("deliberate identity")
            .encode()
            .expect("canonical error encoding");
        for value in containers(deliberate.clone()) {
            let propagated = value.canonical().unwrap_err();
            assert_eq!(propagated, deliberate);
            assert_eq!(
                propagated
                    .canonical_error()
                    .expect("deliberate identity")
                    .encode()
                    .expect("canonical error encoding"),
                expected,
            );
        }
    }

    #[test]
    fn recovery_handlers_can_inspect_error_fields() {
        assert_eq!(
            evaluate_recovery("(1 / 0) |? (failure => failure.code)"),
            text("ORNA-EVAL-DIVIDE-BY-ZERO"),
        );
        assert_eq!(
            evaluate_recovery("(1 / 0) |? (failure => failure.message)"),
            text("<redacted>"),
        );
        assert_eq!(
            evaluate_recovery(
                r#"fail(error(code: "outer", message: "outer", cause: error(code: "inner", message: "inner"))) |? (failure => failure.causes[0].code)"#,
            ),
            text("inner"),
        );
    }

    #[test]
    fn error_intrinsic_rejects_invalid_arguments_and_respects_shadowing() {
        for source in [
            r#"error("code", "message")"#,
            r#"error(code: "only code")"#,
            r#"error(code: "x", message: 1)"#,
            r#"error(code: "x", message: "y", cause: 1)"#,
            r#"error(code: "x", message: "y", extra: "z")"#,
            r#"error(code: "x", message: "y", code: "duplicate")"#,
            r#"1 | error(code: "x", message: "y")"#,
        ] {
            assert!(
                matches!(
                    evaluate_expression(source, &Environment::new(), Limits::default())
                        .unwrap_err()
                        .code(),
                    "ORNA-EVAL-ARGUMENT" | "ORNA-EVAL-TYPE"
                ),
                "{source}"
            );
        }

        assert_eq!(
            evaluate_expression(
                r#"error(code: "x", message: (1 / 0))"#,
                &Environment::new(),
                Limits::default(),
            )
            .unwrap_err()
            .code(),
            "ORNA-EVAL-DIVIDE-BY-ZERO",
        );

        let parsed = orna_syntax_v1::parse_module(
            "fn error(code: Str, message: Str): Int = 7; fn run() = error(code: \"x\", message: \"y\");",
        );
        assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
        let functions = parsed
            .value
            .items
            .into_iter()
            .map(|item| {
                let orna_syntax_v1::Declaration::Function { signature, body } = item.declaration
                else {
                    panic!("function expected")
                };
                (
                    signature.name,
                    PureFunction {
                        parameters: signature.parameters,
                        body,
                        environment: Environment::new(),
                    },
                )
            })
            .collect();
        assert_eq!(
            invoke_named("run", &functions, &Environment::new(), Limits::default()).unwrap(),
            integer(7),
        );
    }

    fn evaluate_handler_value(
        source: &str,
        failure: EvaluationError,
    ) -> Result<Value, EvaluationError> {
        let parsed = parse_expression(source);
        assert!(parsed.is_ok(), "{source} should parse");
        let functions = Functions::new();
        let mut context = Context {
            limits: Limits::default(),
            steps: 0,
            functions: &functions,
            aliases: None,
            session_functions: None,
            repl_bindings: false,
            restrict_function_names: false,
            reject_unhandled_field_calls: false,
            effects: None,
            namespace: None,
            transfer: None,
            cancellation: None,
        };
        let mut scope = Scope(
            BTreeMap::new(),
            BTreeSet::new(),
            BTreeSet::new(),
            NominalDefinitions::new(),
            BTreeSet::new(),
        );
        scope.0.insert("failure".into(), Value::Error(failure));
        context.evaluate(&parsed.value, &mut scope, 0)
    }

    #[test]
    fn recovery_handlers_expose_bounded_safe_details_and_fail_closed() {
        let canonical = CanonicalErrorValue::new(
            "detail.error",
            "private",
            [],
            BTreeMap::from([
                ("attempt".into(), orna_value_v1::Value::int(3.into())),
                (
                    "label".into(),
                    orna_value_v1::Value::new(Raw::Text("safe".into())).unwrap(),
                ),
            ]),
        )
        .unwrap();
        let value = evaluate_handler_value(
            "failure.safe_details.attempt",
            EvaluationError::from_canonical(canonical),
        )
        .unwrap();
        assert_eq!(value.canonical().unwrap(), integer(3));

        let nested = CanonicalErrorValue::new("nested", "private", [], BTreeMap::new()).unwrap();
        let canonical = CanonicalErrorValue::new(
            "detail.error",
            "private",
            [],
            BTreeMap::from([("nested".into(), nested.value().clone())]),
        )
        .unwrap();
        assert_eq!(
            evaluate_handler_value(
                "failure.safe_details.nested",
                EvaluationError::from_canonical(canonical),
            )
            .unwrap_err()
            .code(),
            "ORNA-EVAL-UNSUPPORTED",
        );
    }

    fn evaluate_with_safe_details_effects(
        source: &str,
        effects: &mut SafeDetailsEffects,
    ) -> Result<Value, EvaluationError> {
        let parsed = parse_expression(source);
        assert!(parsed.is_ok(), "{source} should parse");
        let functions = Functions::new();
        let mut context = Context {
            limits: Limits::default(),
            steps: 0,
            functions: &functions,
            aliases: None,
            session_functions: None,
            repl_bindings: false,
            restrict_function_names: false,
            reject_unhandled_field_calls: false,
            effects: Some(effects),
            namespace: None,
            transfer: None,
            cancellation: None,
        };
        let mut scope = Scope(
            BTreeMap::new(),
            BTreeSet::new(),
            BTreeSet::new(),
            NominalDefinitions::new(),
            BTreeSet::new(),
        );
        context.evaluate(&parsed.value, &mut scope, 0)
    }

    #[test]
    fn recovery_reads_safe_details_through_an_actual_effect_boundary() {
        let scalar = CanonicalErrorValue::new(
            "detail.error",
            "private",
            [],
            BTreeMap::from([("attempt".into(), orna_value_v1::Value::int(3.into()))]),
        )
        .unwrap();
        let mut effects = SafeDetailsEffects {
            failure: EvaluationError::from_canonical(scalar),
        };
        let value = evaluate_with_safe_details_effects(
            "probe.error() |? (failure => failure.safe_details.attempt)",
            &mut effects,
        )
        .expect("safe scalar detail should be readable");
        assert_eq!(value.canonical().unwrap(), integer(3));

        let nested = CanonicalErrorValue::new("nested", "private", [], BTreeMap::new()).unwrap();
        let canonical = CanonicalErrorValue::new(
            "detail.error",
            "private",
            [],
            BTreeMap::from([("nested".into(), nested.value().clone())]),
        )
        .unwrap();
        let mut effects = SafeDetailsEffects {
            failure: EvaluationError::from_canonical(canonical),
        };
        let error = evaluate_with_safe_details_effects(
            "probe.error() |? (failure => failure.safe_details.nested)",
            &mut effects,
        )
        .unwrap_err();
        assert_eq!(error.code(), "ORNA-EVAL-UNSUPPORTED");
    }

    #[test]
    fn error_field_access_rejects_missing_and_unknown_fields() {
        let failure = EvaluationError::from_canonical(
            CanonicalErrorValue::new("code", "message", [], BTreeMap::new()).unwrap(),
        );
        for source in ["failure.cause", "failure.unknown"] {
            assert_eq!(
                evaluate_handler_value(source, failure.clone())
                    .unwrap_err()
                    .code(),
                "ORNA-EVAL-FIELD",
                "{source}"
            );
        }
    }

    #[test]
    fn recovery_pipelines_propagate_handler_failures_to_the_next_boundary() {
        assert_eq!(
            evaluate_expression(
                "(1 / 0) |? (failure => missing)",
                &Environment::new(),
                Limits::default(),
            )
            .unwrap_err()
            .code(),
            "ORNA-EVAL-NAME",
        );
        assert_eq!(
            evaluate_recovery("(1 / 0) |? (failure => 2 / 0) |? (failure => 7)"),
            integer(7),
        );
    }

    struct RecoveryEffects {
        cancellation: Option<CancellationToken>,
        recovery_calls: usize,
    }

    impl EffectHandler for RecoveryEffects {
        fn handle(
            &mut self,
            callee: &Expr,
            _: &[CanonicalValue],
        ) -> Result<Option<CanonicalValue>, EvaluationError> {
            match function_name(callee).as_deref() {
                Some("probe.cancel") => {
                    self.cancellation
                        .as_ref()
                        .expect("cancellation probe has a token")
                        .request();
                    Ok(Some(integer(1)))
                }
                Some("probe.count") => {
                    self.recovery_calls += 1;
                    Ok(Some(integer(7)))
                }
                _ => Ok(None),
            }
        }
    }

    struct SafeDetailsEffects {
        failure: EvaluationError,
    }

    impl EffectHandler for SafeDetailsEffects {
        fn handle(
            &mut self,
            callee: &Expr,
            _: &[CanonicalValue],
        ) -> Result<Option<CanonicalValue>, EvaluationError> {
            if function_name(callee).as_deref() == Some("probe.error") {
                return Err(self.failure.clone());
            }
            Ok(None)
        }
    }

    fn evaluate_with_recovery_effects(
        source: &str,
        cancellation: Option<&CancellationToken>,
        effects: &mut RecoveryEffects,
    ) -> Result<Value, EvaluationError> {
        let parsed = parse_expression(source);
        assert!(parsed.is_ok(), "{source} should parse");
        let functions = Functions::new();
        let mut context = Context {
            limits: Limits::default(),
            steps: 0,
            functions: &functions,
            aliases: None,
            session_functions: None,
            repl_bindings: false,
            restrict_function_names: false,
            reject_unhandled_field_calls: false,
            effects: Some(effects),
            namespace: None,
            transfer: None,
            cancellation,
        };
        let mut scope = Scope(
            BTreeMap::new(),
            BTreeSet::new(),
            BTreeSet::new(),
            NominalDefinitions::new(),
            BTreeSet::new(),
        );
        context.evaluate(&parsed.value, &mut scope, 0)
    }

    #[test]
    fn recovery_does_not_deliver_cancellation_to_handler() {
        let cancellation = CancellationToken::new();
        let mut effects = RecoveryEffects {
            cancellation: Some(cancellation.clone()),
            recovery_calls: 0,
        };
        let result = evaluate_with_recovery_effects(
            "(probe.cancel() / 0) |? (failure => probe.count())",
            Some(&cancellation),
            &mut effects,
        );

        assert_eq!(result.unwrap_err().code(), "ORNA-EVAL-CANCELLED");
        assert_eq!(effects.recovery_calls, 0);
    }

    #[test]
    fn recovery_internal_error_cannot_cross_canonical_boundary() {
        let error = evaluate_expression(
            "(1 / 0) |? (failure => failure)",
            &Environment::new(),
            Limits::default(),
        )
        .unwrap_err();

        assert_eq!(error.code(), "ORNA-EVAL-DIVIDE-BY-ZERO");
        assert_eq!(error.diagnostic().message(), "<redacted>");
        assert!(error.canonical_error().is_none());
    }

    #[test]
    fn recovery_handler_is_invoked_once_for_one_ordinary_failure() {
        let mut effects = RecoveryEffects {
            cancellation: None,
            recovery_calls: 0,
        };
        let result = evaluate_with_recovery_effects(
            "(1 / 0) |? (failure => probe.count())",
            None,
            &mut effects,
        )
        .expect("ordinary failure should be recovered");

        assert_eq!(
            result.canonical().expect("probe result is canonical"),
            integer(7)
        );
        assert_eq!(effects.recovery_calls, 1);
    }

    struct RelationEffects {
        calls: usize,
    }

    impl EffectHandler for RelationEffects {
        fn handle(
            &mut self,
            callee: &Expr,
            _: &[CanonicalValue],
        ) -> Result<Option<CanonicalValue>, EvaluationError> {
            if matches!(callee, Expr::Field { base, name, .. }
                if matches!(base.as_ref(), Expr::ReplBinding { text, .. } if text == "$__orna_relation")
                    && name == "integer_aggregate")
            {
                self.calls += 1;
                return Ok(Some(
                    CanonicalValue::new(Raw::Int(7.into())).expect("integer is canonical"),
                ));
            }
            Ok(None)
        }
    }

    struct NonAdvancingRelationEffects;

    impl EffectHandler for NonAdvancingRelationEffects {
        fn handle(
            &mut self,
            _: &Expr,
            _: &[CanonicalValue],
        ) -> Result<Option<CanonicalValue>, EvaluationError> {
            Ok(None)
        }

        fn scan_relation_page(
            &mut self,
            _: &str,
            after: Option<&[u8]>,
            _: usize,
            budget: &mut StepBudget,
        ) -> Result<Option<RelationPage>, EvaluationError> {
            budget.debit(1)?;
            let value = if after.is_some() { 8 } else { 7 };
            Ok(Some(RelationPage {
                rows: vec![
                    CanonicalValue::new(Raw::Int(value.into())).expect("integer is canonical"),
                ],
                next: Some(vec![1]),
            }))
        }
    }

    #[test]
    fn synthetic_relation_receiver_is_a_static_effect_path() {
        let span = orna_syntax_v1::SyntaxSpan::new(0, 0);
        let expression = Expr::Call {
            callee: Box::new(Expr::Field {
                base: Box::new(Expr::ReplBinding {
                    text: "$__orna_relation".into(),
                    span: span.clone(),
                }),
                name: "integer_aggregate".into(),
                span: span.clone(),
            }),
            arguments: Vec::new(),
            span,
        };
        let functions = Functions::new();
        let mut effects = RelationEffects { calls: 0 };
        let mut context = Context {
            limits: Limits::default(),
            steps: 0,
            functions: &functions,
            aliases: None,
            session_functions: None,
            repl_bindings: true,
            restrict_function_names: true,
            reject_unhandled_field_calls: true,
            effects: Some(&mut effects),
            namespace: None,
            transfer: None,
            cancellation: None,
        };
        let mut scope = Scope(
            BTreeMap::new(),
            BTreeSet::new(),
            BTreeSet::new(),
            NominalDefinitions::new(),
            BTreeSet::new(),
        );

        assert_eq!(
            context.evaluate(&expression, &mut scope, 0).unwrap(),
            Value::Int(7.into())
        );
        drop(context);
        assert_eq!(effects.calls, 1);
    }

    #[test]
    fn nonadvancing_relation_cursor_rejects_page_before_callback() {
        let functions = Functions::new();
        let mut effects = NonAdvancingRelationEffects;
        let mut context = Context {
            limits: Limits::default(),
            steps: 0,
            functions: &functions,
            aliases: None,
            session_functions: None,
            repl_bindings: true,
            restrict_function_names: true,
            reject_unhandled_field_calls: true,
            effects: Some(&mut effects),
            namespace: None,
            transfer: None,
            cancellation: None,
        };
        let plan = RelationPlan::new("Note".into());
        let mut callbacks = 0;

        let result = context.for_each_relation_value(&plan, 0, |_, _| {
            callbacks += 1;
            Ok(true)
        });

        assert_eq!(result.unwrap_err().code(), "ORNA-EVAL-VALUE");
        assert_eq!(callbacks, 1);
    }

    #[test]
    fn relation_plan_fuses_adjacent_filters_in_order_without_crossing_map() {
        let first = Value::Bool(true);
        let second = Value::Bool(false);
        let transform = Value::Int(7.into());
        let third = Value::Bool(true);
        let plan = RelationPlan::new("Note".into())
            .with_stage(RelationStage::Filter(vec![first.clone()]))
            .with_stage(RelationStage::Filter(vec![second.clone()]))
            .with_stage(RelationStage::Map(transform.clone()))
            .with_stage(RelationStage::Filter(vec![third.clone()]))
            .with_stage(RelationStage::Take(2));

        assert_eq!(
            plan.stages,
            vec![
                RelationStage::Filter(vec![first, second]),
                RelationStage::Map(transform),
                RelationStage::Filter(vec![third]),
                RelationStage::Take(2),
            ]
        );
    }

    #[test]
    fn relation_plan_pushes_filter_cascade_into_union_children_in_order() {
        let left_first = Value::Bool(true);
        let right_map = Value::Int(7.into());
        let outer_first = Value::Bool(false);
        let outer_second = Value::Bool(true);
        let left = RelationPlan::new("Left".into())
            .with_stage(RelationStage::Filter(vec![left_first.clone()]));
        let right =
            RelationPlan::new("Right".into()).with_stage(RelationStage::Map(right_map.clone()));
        let plan = RelationPlan::union(left, right)
            .with_stage(RelationStage::Filter(vec![outer_first.clone()]))
            .with_stage(RelationStage::Filter(vec![outer_second.clone()]))
            .with_stage(RelationStage::Take(2));

        assert_eq!(plan.stages, vec![RelationStage::Take(2)]);
        let (left, right) = plan.source_union.as_ref().unwrap();
        assert_eq!(
            left.stages,
            vec![RelationStage::Filter(vec![
                left_first,
                outer_first.clone(),
                outer_second.clone()
            ])]
        );
        assert_eq!(
            right.stages,
            vec![
                RelationStage::Map(right_map),
                RelationStage::Filter(vec![outer_first, outer_second]),
            ]
        );
    }

    #[test]
    fn relation_plan_pushes_ordered_cascade_through_zero_drop_unknown_union_storm() {
        let left_first = Value::Bool(true);
        let right_map = Value::Int(7.into());
        let right_first = Value::Bool(false);
        let outer_first = Value::Bool(false);
        let outer_second = Value::Bool(true);
        let left = RelationPlan::new("Known".into())
            .with_stage(RelationStage::Filter(vec![left_first.clone()]));
        let unknown_left = RelationPlan::new("UnknownLeft".into())
            .with_stage(RelationStage::Map(right_map.clone()));
        let unknown_right = RelationPlan::new("UnknownRight".into())
            .with_stage(RelationStage::Filter(vec![right_first.clone()]));
        let plan = RelationPlan::union(left, RelationPlan::union(unknown_left, unknown_right))
            .with_stage(RelationStage::Drop(0))
            .with_stage(RelationStage::Filter(vec![outer_first.clone()]))
            .with_stage(RelationStage::Filter(vec![outer_second.clone()]))
            .with_stage(RelationStage::Take(2));

        assert_eq!(plan.stages, vec![RelationStage::Take(2)]);
        let (left, right) = plan.source_union.as_ref().unwrap();
        assert_eq!(
            left.stages,
            vec![RelationStage::Filter(vec![
                left_first,
                outer_first.clone(),
                outer_second.clone(),
            ])]
        );
        let (unknown_left, unknown_right) = right.source_union.as_ref().unwrap();
        assert_eq!(
            unknown_left.stages,
            vec![
                RelationStage::Map(right_map),
                RelationStage::Filter(vec![outer_first.clone(), outer_second.clone()]),
            ]
        );
        assert_eq!(
            unknown_right.stages,
            vec![RelationStage::Filter(vec![
                right_first,
                outer_first,
                outer_second,
            ])]
        );
    }

    #[test]
    fn relation_plan_batches_adjacent_filters_before_unknown_union_storm_flush() {
        let existing = Value::Bool(true);
        let transform = Value::Int(7.into());
        let first = Value::Bool(false);
        let second = Value::Bool(true);
        let third = Value::Int(3.into());
        let plan = RelationPlan::union(
            RelationPlan::new("Known".into())
                .with_stage(RelationStage::Filter(vec![existing.clone()])),
            RelationPlan::union(
                RelationPlan::new("UnknownMapped".into())
                    .with_stage(RelationStage::Map(transform.clone())),
                RelationPlan::union(
                    RelationPlan::new("UnknownLeft".into())
                        .with_stage(RelationStage::Filter(vec![existing.clone()])),
                    RelationPlan::new("UnknownRight".into()),
                ),
            ),
        )
        .with_stage(RelationStage::Drop(0))
        .with_stage(RelationStage::Filter(vec![first.clone()]))
        .with_stage(RelationStage::Filter(vec![second.clone()]))
        .with_stage(RelationStage::Filter(vec![third.clone()]));

        assert_eq!(
            plan.stages,
            vec![RelationStage::Filter(vec![
                first.clone(),
                second.clone(),
                third.clone(),
            ])],
            "adjacent filters compile into one pending cascade before tree traversal"
        );
        let flushed = plan.flush_filter_cascade();
        assert!(flushed.stages.is_empty());
        let (known, storm) = flushed.source_union.as_ref().unwrap();
        assert_eq!(
            known.stages,
            vec![RelationStage::Filter(vec![
                existing.clone(),
                first.clone(),
                second.clone(),
                third.clone(),
            ])]
        );
        let (mapped, nested) = storm.source_union.as_ref().unwrap();
        assert_eq!(
            mapped.stages,
            vec![
                RelationStage::Map(transform),
                RelationStage::Filter(vec![first.clone(), second.clone(), third.clone()]),
            ],
            "an intervening map remains before the pending cascade"
        );
        let (unknown_left, unknown_right) = nested.source_union.as_ref().unwrap();
        assert_eq!(
            unknown_left.stages,
            vec![RelationStage::Filter(vec![
                existing,
                first.clone(),
                second.clone(),
                third.clone(),
            ])]
        );
        assert_eq!(
            unknown_right.stages,
            vec![RelationStage::Filter(vec![first, second, third])]
        );
    }

    #[test]
    fn relation_plan_flushes_operand_batch_before_nested_union_composition() {
        let first = Value::Bool(true);
        let second = Value::Bool(false);
        let third = Value::Int(3.into());
        let fourth = Value::Int(4.into());
        let inner = RelationPlan::union(
            RelationPlan::new("KnownLeft".into()),
            RelationPlan::new("UnknownRight".into()),
        )
        .with_stage(RelationStage::Filter(vec![first.clone()]))
        .with_stage(RelationStage::Filter(vec![second.clone()]));

        assert_eq!(
            inner.stages,
            vec![RelationStage::Filter(vec![first.clone(), second.clone()])]
        );
        let outer = RelationPlan::union(inner, RelationPlan::new("UnknownTail".into()))
            .with_stage(RelationStage::Filter(vec![third.clone()]))
            .with_stage(RelationStage::Filter(vec![fourth.clone()]))
            .flush_filter_cascade();

        assert!(outer.stages.is_empty());
        let (inner, unknown_tail) = outer.source_union.as_ref().unwrap();
        assert!(inner.stages.is_empty());
        let (known_left, unknown_right) = inner.source_union.as_ref().unwrap();
        let merged = vec![first, second, third.clone(), fourth.clone()];
        assert_eq!(
            known_left.stages,
            vec![RelationStage::Filter(merged.clone())]
        );
        assert_eq!(unknown_right.stages, vec![RelationStage::Filter(merged)]);
        assert_eq!(
            unknown_tail.stages,
            vec![RelationStage::Filter(vec![third, fourth])]
        );
    }

    #[test]
    fn relation_plan_shares_persistent_outer_batch_across_unknown_union_leaves() {
        let left_own = Value::Bool(true);
        let right_map = Value::Int(7.into());
        let middle_own = Value::Bool(false);
        let outer_first = Value::Int(8.into());
        let outer_second = Value::Int(9.into());
        let outer_third = Value::Int(10.into());
        let plan = RelationPlan::union(
            RelationPlan::union(
                RelationPlan::new("UnknownLeft".into())
                    .with_stage(RelationStage::Filter(vec![left_own.clone()])),
                RelationPlan::new("UnknownMiddle".into())
                    .with_stage(RelationStage::Map(right_map.clone()))
                    .with_stage(RelationStage::Filter(vec![middle_own.clone()])),
            ),
            RelationPlan::new("UnknownRight".into()),
        )
        .with_stage(RelationStage::Filter(vec![outer_first.clone()]))
        .with_stage(RelationStage::Filter(vec![outer_second.clone()]))
        .with_stage(RelationStage::Filter(vec![outer_third.clone()]))
        .flush_filter_cascade();

        let (left, right) = plan.source_union.as_ref().expect("outer union remains");
        let (left_leaf, middle_leaf) = left.source_union.as_ref().expect("nested union remains");
        let batch_for = |leaf: &RelationPlan| match leaf.stages.as_slice() {
            [RelationStage::Map(_), RelationStage::SharedFilter(batch)]
            | [RelationStage::SharedFilter(batch)] => Arc::clone(batch),
            stages => panic!("expected one compiled leaf batch, got {stages:?}"),
        };
        let left_batch = batch_for(left_leaf);
        let middle_batch = batch_for(middle_leaf);
        let right_batch = batch_for(right);

        assert_eq!(
            left_batch
                .chunks()
                .iter()
                .flat_map(|chunk| chunk.iter())
                .cloned()
                .collect::<Vec<_>>(),
            vec![
                left_own,
                outer_first.clone(),
                outer_second.clone(),
                outer_third.clone()
            ]
        );
        assert_eq!(
            middle_batch
                .chunks()
                .iter()
                .flat_map(|chunk| chunk.iter())
                .cloned()
                .collect::<Vec<_>>(),
            vec![middle_own, outer_first, outer_second, outer_third]
        );
        let left_shared_suffix = left_batch.chunks().last().expect("outer batch suffix");
        let middle_shared_suffix = middle_batch.chunks().last().expect("outer batch suffix");
        let right_shared_batch = right_batch.chunks().first().expect("outer batch");
        assert!(Arc::ptr_eq(left_shared_suffix, middle_shared_suffix));
        assert!(Arc::ptr_eq(left_shared_suffix, right_shared_batch));
        assert_eq!(middle_leaf.stages[0], RelationStage::Map(right_map));
    }

    #[test]
    fn relation_plan_reuses_unknown_operand_batch_when_outer_union_composes() {
        let unknown_first = Value::Bool(true);
        let unknown_second = Value::Bool(false);
        let outer_first = Value::Int(8.into());
        let outer_second = Value::Int(9.into());
        let unknown_batch = RelationPlan::union(
            RelationPlan::new("UnknownLeft".into()),
            RelationPlan::new("UnknownRight".into()),
        )
        .with_stage(RelationStage::Filter(vec![unknown_first.clone()]))
        .with_stage(RelationStage::Filter(vec![unknown_second.clone()]));
        let plan = RelationPlan::union(unknown_batch, RelationPlan::new("UnknownTail".into()))
            .with_stage(RelationStage::Filter(vec![outer_first.clone()]))
            .with_stage(RelationStage::Filter(vec![outer_second.clone()]))
            .flush_filter_cascade();

        let (unknown_union, unknown_tail) = plan.source_union.as_ref().expect("outer union");
        let (left, right) = unknown_union
            .source_union
            .as_ref()
            .expect("unknown operand union");
        let batch_for = |leaf: &RelationPlan| match leaf.stages.as_slice() {
            [RelationStage::SharedFilter(batch)] => Arc::clone(batch),
            stages => panic!("expected one shared batch, got {stages:?}"),
        };
        let left_batch = batch_for(left);
        let right_batch = batch_for(right);
        assert!(Arc::ptr_eq(&left_batch, &right_batch));
        assert_eq!(
            left_batch.values().cloned().collect::<Vec<_>>(),
            vec![
                unknown_first,
                unknown_second,
                outer_first.clone(),
                outer_second.clone()
            ]
        );
        let tail_batch = batch_for(unknown_tail);
        assert_eq!(
            tail_batch.values().cloned().collect::<Vec<_>>(),
            vec![outer_first, outer_second]
        );
    }

    #[test]
    fn relation_plan_reuses_shared_union_filter_join_across_batch_flushes() {
        let local_filter = Value::Bool(true);
        let outer_filter = Value::Bool(false);
        let operand = RelationPlan::union(
            RelationPlan::new("UnknownLeft".into()),
            RelationPlan::new("UnknownRight".into()),
        )
        .with_stage(RelationStage::Filter(vec![local_filter]));
        let operand = operand.flush_filter_cascade();
        let cascade = RelationPlan::union(
            RelationPlan::new("CascadeLeft".into()),
            RelationPlan::new("CascadeRight".into()),
        )
        .with_stage(RelationStage::Filter(vec![outer_filter]));
        let cascade = cascade.flush_filter_cascade();
        let batch_for = |leaf: &RelationPlan| match leaf.stages.as_slice() {
            [RelationStage::SharedFilter(batch)] => Arc::clone(batch),
            stages => panic!("expected one shared batch, got {stages:?}"),
        };
        let shared_cascade = batch_for(cascade.source_union.as_ref().unwrap().0.as_ref());
        let compose = || {
            operand
                .clone()
                .with_stage(RelationStage::SharedFilter(Arc::clone(&shared_cascade)))
                .flush_filter_cascade()
        };
        let first = compose();
        let second = compose();
        let composed_left = first.source_union.as_ref().unwrap().0.as_ref();
        let repeated_left = second.source_union.as_ref().unwrap().0.as_ref();
        assert!(Arc::ptr_eq(
            &batch_for(composed_left),
            &batch_for(repeated_left)
        ));
    }

    #[test]
    fn relation_plan_reuses_equal_cloned_prefixes_on_shared_union_batch() {
        let local_filter = Value::Bool(true);
        let outer_filter = Value::Bool(false);
        let operand = RelationPlan::new("UnknownOperand".into())
            .with_stage(RelationStage::Filter(vec![local_filter.clone()]));
        let cascade = RelationPlan::union(
            RelationPlan::new("CascadeLeft".into()),
            RelationPlan::new("CascadeRight".into()),
        )
        .with_stage(RelationStage::Filter(vec![outer_filter.clone()]))
        .flush_filter_cascade();
        let batch_for = |leaf: &RelationPlan| match leaf.stages.as_slice() {
            [RelationStage::SharedFilter(batch)] => Arc::clone(batch),
            stages => panic!("expected one shared batch, got {stages:?}"),
        };
        let shared_outer = batch_for(cascade.source_union.as_ref().unwrap().0.as_ref());
        let composed = RelationPlan::union(operand.clone(), operand)
            .with_stage(RelationStage::SharedFilter(shared_outer))
            .flush_filter_cascade();
        let (left, right) = composed
            .source_union
            .as_ref()
            .expect("unknown union remains");
        let left_batch = batch_for(left);
        let right_batch = batch_for(right);

        assert!(Arc::ptr_eq(&left_batch, &right_batch));
        assert_eq!(
            left_batch.values().cloned().collect::<Vec<_>>(),
            vec![local_filter, outer_filter]
        );
    }

    #[test]
    fn relation_plan_shares_cloned_prefixes_across_nested_unknown_union_leaves() {
        let local_filter = Value::Bool(true);
        let outer_filter = Value::Bool(false);
        let operand = RelationPlan::new("UnknownOperand".into())
            .with_stage(RelationStage::Filter(vec![local_filter.clone()]));
        let cascade = RelationPlan::union(
            RelationPlan::new("CascadeLeft".into()),
            RelationPlan::new("CascadeRight".into()),
        )
        .with_stage(RelationStage::Filter(vec![outer_filter.clone()]))
        .flush_filter_cascade();
        let batch_for = |leaf: &RelationPlan| match leaf.stages.as_slice() {
            [RelationStage::SharedFilter(batch)] => Arc::clone(batch),
            stages => panic!("expected one shared batch, got {stages:?}"),
        };
        let shared_outer = batch_for(cascade.source_union.as_ref().unwrap().0.as_ref());
        let composed = RelationPlan::union(
            RelationPlan::union(operand.clone(), operand.clone()),
            operand,
        )
        .with_stage(RelationStage::SharedFilter(shared_outer))
        .flush_filter_cascade();
        let (nested, right) = composed.source_union.as_ref().expect("outer union remains");
        let (left, middle) = nested.source_union.as_ref().expect("nested union remains");
        let left_batch = batch_for(left);
        let middle_batch = batch_for(middle);
        let right_batch = batch_for(right);

        assert!(Arc::ptr_eq(&left_batch, &middle_batch));
        assert!(Arc::ptr_eq(&left_batch, &right_batch));
        assert_eq!(
            left_batch.values().cloned().collect::<Vec<_>>(),
            vec![local_filter, outer_filter]
        );
    }

    #[test]
    fn relation_scan_checks_cancellation_before_each_page() {
        let functions = Functions::new();
        let cancellation = CancellationToken::new();
        cancellation.request_after_checks(1);
        let mut effects = NonAdvancingRelationEffects;
        let mut context = Context {
            limits: Limits::default(),
            steps: 0,
            functions: &functions,
            aliases: None,
            session_functions: None,
            repl_bindings: true,
            restrict_function_names: true,
            reject_unhandled_field_calls: true,
            effects: Some(&mut effects),
            namespace: None,
            transfer: None,
            cancellation: Some(&cancellation),
        };
        let plan = RelationPlan::new("Note".into());
        let mut callbacks = 0;

        let result = context.for_each_relation_value(&plan, 0, |_, _| {
            callbacks += 1;
            Ok(true)
        });

        assert_eq!(result.unwrap_err().code(), "ORNA-EVAL-CANCELLED");
        assert_eq!(callbacks, 0);
    }

    #[test]
    fn relation_scan_checks_cancellation_before_each_row_callback() {
        let functions = Functions::new();
        let cancellation = CancellationToken::new();
        cancellation.request_after_checks(2);
        let mut effects = NonAdvancingRelationEffects;
        let mut context = Context {
            limits: Limits::default(),
            steps: 0,
            functions: &functions,
            aliases: None,
            session_functions: None,
            repl_bindings: true,
            restrict_function_names: true,
            reject_unhandled_field_calls: true,
            effects: Some(&mut effects),
            namespace: None,
            transfer: None,
            cancellation: Some(&cancellation),
        };
        let plan = RelationPlan::new("Note".into());
        let mut callbacks = 0;

        let result = context.for_each_relation_value(&plan, 0, |_, _| {
            callbacks += 1;
            Ok(true)
        });

        assert_eq!(result.unwrap_err().code(), "ORNA-EVAL-CANCELLED");
        assert_eq!(callbacks, 0);
    }

    #[test]
    fn buffered_relation_stages_check_cancellation_before_each_value() {
        let functions = Functions::new();
        let cancellation = CancellationToken::new();
        cancellation.request_after_checks(2);
        let mut context = Context {
            limits: Limits::default(),
            steps: 0,
            functions: &functions,
            aliases: None,
            session_functions: None,
            repl_bindings: true,
            restrict_function_names: true,
            reject_unhandled_field_calls: true,
            effects: None,
            namespace: None,
            transfer: None,
            cancellation: Some(&cancellation),
        };
        let mut callbacks = 0;
        let mut distinct_seen = Vec::new();
        let mut pair_previous = Vec::new();
        let mut window_states = Vec::new();

        let result = context.for_each_buffered_stages(
            vec![Value::Int(1.into()), Value::Int(2.into())],
            &[],
            0,
            0,
            &mut distinct_seen,
            &mut pair_previous,
            &mut window_states,
            |_, _| {
                callbacks += 1;
                Ok(true)
            },
        );

        assert_eq!(result.unwrap_err().code(), "ORNA-EVAL-CANCELLED");
        assert_eq!(callbacks, 1);
    }

    #[test]
    fn source_ui_action_constructs_canonical_typed_descriptor() {
        let value = evaluate_expression(
            r#"std.ui.action("save", as: Text, debug_kind: "button")"#,
            &Environment::new(),
            Limits::default(),
        )
        .expect("source action descriptor");
        let Raw::Map(fields) = value.raw() else {
            panic!("action descriptor must be a canonical map");
        };
        let field = |name: &str| {
            fields
                .iter()
                .find_map(|(key, value)| match key {
                    Raw::Text(key) if key == name => Some(value),
                    _ => None,
                })
                .expect("descriptor field")
        };
        assert_eq!(field("action_id"), &Raw::Text("save".into()));
        assert_eq!(field("input_type"), &Raw::Text("std.text".into()));
        assert_eq!(field("debug_kind"), &Raw::Text("button".into()));
    }

    #[test]
    fn presentation_container_validation_rejects_incomplete_nested_contracts() {
        fn node(children: Vec<Value>) -> Value {
            Value::Record(BTreeMap::from([
                ("kind".into(), Value::String("node".into())),
                (
                    "contract".into(),
                    Value::Record(BTreeMap::from([
                        ("id".into(), Value::String("std.ui.text@1".into())),
                        ("name".into(), Value::String("std.ui.text".into())),
                        ("version".into(), Value::String("1.0".into())),
                    ])),
                ),
                ("properties".into(), Value::Record(BTreeMap::new())),
                (
                    "slots".into(),
                    Value::Record(BTreeMap::from([("content".into(), Value::List(children))])),
                ),
                ("actions".into(), Value::Record(BTreeMap::new())),
            ]))
        }

        let valid = node(Vec::new());
        assert!(is_ui_presentation_node(&valid, 0, 1).expect("shallow node depth"));

        let nested = node(vec![valid.clone()]);
        assert!(is_ui_presentation_node(&nested, 0, 1).expect("child at depth one"));
        assert_eq!(
            is_ui_presentation_node(&nested, 0, 0)
                .expect_err("depth zero must reject its child")
                .code(),
            "ORNA-EVAL-LIMIT"
        );

        let mut invalid_child = node(Vec::new());
        let Value::Record(fields) = &mut invalid_child else {
            unreachable!();
        };
        fields.insert("contract".into(), Value::Record(BTreeMap::new()));

        let parent = node(vec![invalid_child]);
        assert!(!is_ui_presentation_node(&parent, 0, 2).expect("invalid child is not too deep"));
    }

    #[test]
    fn presentation_optional_types_preserve_nested_type_paths() {
        assert!(ui_property_type_matches(
            "std.option<std.option<app.Token>>",
            &Value::Option(None)
        ));
        assert!(!ui_property_type_matches(
            "std.option<>",
            &Value::Option(None)
        ));
        assert!(!ui_property_type_matches(
            "std.option<std.option<>>",
            &Value::Null
        ));
        assert!(!ui_property_type_matches(
            "std.option<std..text>",
            &Value::Null
        ));
    }

    #[test]
    fn non_terminating_decimal_division_reports_its_typed_failure_from_source() {
        let source = include_str!("../tests/fixtures/consumer_gap.orna");
        let failure = evaluate_expression(source, &Environment::new(), Limits::default())
            .expect_err("one third has no finite decimal representation");

        assert_eq!(failure.code(), "InexactDivision");
    }

    #[test]
    fn source_while_and_loop_execute_with_continue_break_and_step_bounds() {
        let source = include_str!("../tests/fixtures/control_flow_loop_gap.orna");
        let parsed = parse_expression(source);
        assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
        let value = evaluate_expression(source, &Environment::new(), Limits::default())
            .expect("while statement and loop value should evaluate");
        assert_eq!(value.raw(), &Raw::Int(5.into()));

        let limits = Limits {
            max_steps: 4,
            ..Limits::default()
        };
        let failure = evaluate_expression(source, &Environment::new(), limits)
            .expect_err("the same loop must stop when its activation budget is exhausted");
        assert_eq!(failure.code(), "ORNA-EVAL-LIMIT");
    }
}
