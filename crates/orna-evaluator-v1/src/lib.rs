//! A deliberately small, deterministic Orna 1.0 expression evaluator.
//!
//! The legacy public boundary admits and returns OVB-1 canonical values. The
//! separate [`evaluate_expression_ovb2`] boundary admits and returns OVB-2
//! context values, preserving annotated Blobs without changing OVB-1 APIs.
//! The evaluator has no I/O, external mutation, clock, random, module-loading,
//! or host-call capability.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet, VecDeque},
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
    CANONICAL_NAN_BITS, ContextValue, Error as ValueError, ErrorValue as CanonicalErrorValue,
    Format3Context, OVB2_BLOB_TAG, ROV3_BLOB_TAG, SOV3_BLOB_TAG, Raw, ValueFormat,
    domain_digest, float_max, float_min, float_ordinary_eq, float_total_cmp,
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
mod list;
mod regex_lite;
mod relation;
mod repl;
mod source_enums;
#[cfg(feature = "system-host-bindings")]
mod sys_bindings;
#[cfg(feature = "system-host-bindings")]
mod explain_bindings;
#[cfg(feature = "system-host-bindings")]
mod runtime_info;
mod timezone;
mod ui;
mod unicode_16_case_properties;
mod browser_profile;
pub mod row_admission;
pub use browser_profile::{BrowserProfileError, execute_browser_profile};
#[cfg(feature = "project-repl")]
pub use admitted_repl::{
    AdmittedReplSession, ReplDetail, ReplError, RepositoryScope, StagedReplActivation,
    effect_rejection,
};
pub use cancellation::CancellationToken;
use list::List;
use relation::{
    BucketBySpec, BucketPeriod, RelationBucket, RelationBucketError, RelationBucketState,
    RelationLastState, RelationPlan, RelationStage, RelationWindowState,
};
pub use relation::{RelationPlanMetadata, RelationPlanNode};
pub use repl::{ReplSession, parse_admitted_repl};
pub use source_enums::source_enum_definitions;
#[cfg(feature = "system-host-bindings")]
pub use sys_bindings::SysHostBindingRegistry;
#[cfg(feature = "system-host-bindings")]
pub use orna_sys_v1::SysHostContext;
#[cfg(feature = "system-host-bindings")]
pub use runtime_info::SysHostAttachment;
#[cfg(feature = "system-host-bindings")]
pub use explain_bindings::{ActivationExplainCatalogue, ExplainCatalogue};
pub use timezone::{
    Instant, LocalDateTime, LocalTimeResolution, TIMEZONE_DATASET_VERSION, TimeZone, TimeZoneError,
    ZonedLocalDateTime, resolve_time_zone,
};

/// The verified standard-source bundle used by the bounded local and remote
/// REPL boundaries. `orna-standard-sources` owns the canonical module source;
/// this crate verifies its pinned profile before either boundary admits an import.
/// Returns the reference standard sources supplied to the bounded REPL.
#[must_use]
pub fn reference_standard_sources() -> [(String, String); 77] {
    orna_standard_sources::reference_standard_sources_v1()
}

/// Returns the immutable profile that verifies [`reference_standard_sources`].
#[must_use]
pub fn reference_standard_profile() -> StandardDependencyProfile {
    StandardDependencyProfile::from_sources(
        "orna.std/v1-reference-library",
        orna_standard_sources::reference_standard_sources_v1(),
    )
    .expect("the bundled Orna 1.0.0 standard module path is valid")
    .with_module_prelude_exports(
        orna_standard_sources::REFERENCE_STANDARD_PRELUDE_PATH_V1,
        orna_standard_sources::REFERENCE_STANDARD_PRELUDE_EXPORTS_V1.iter().copied(),
    )
}

const DEFAULT_SOURCE_BYTES: usize = 65_536;
// Budget for one full-size collection operation at the item limit: 200 steps
// per item plus a fixed allowance (419,600 steps). The pinned std helpers in
// tests/standard_library_size_budget.rs stay within 200 steps per element at
// 1,000, 2,048 and 10,000 elements, because appends reuse list storage and
// native sorts are O(n log n). No quadratic term is needed, so a runaway loop
// fails six times sooner than with the former n²/2 allowance.
const DEFAULT_STEPS: u64 = DEFAULT_ITEMS as u64 * 200 + 10_000;
const DEFAULT_DEPTH: usize = 64;
const DEFAULT_ITEMS: usize = 2_048;
/// Rows requested per storage-backed relation page. Evaluation stays demand-driven: each consumer draws exactly the rows it needs, so a failure in an unread tail is never observed. The runtime caps a page at 1,024 rows (InvalidPageSize above that).
const RELATION_SCAN_PAGE_ROWS: usize = 1;
const DEFAULT_STRING_BYTES: usize = 16_384;
const DEFAULT_INTEGER_DIGITS: usize = 1_024;
/// Native stack headroom kept before evaluation requests a fresh stack segment.
const EVALUATOR_STACK_RED_ZONE: usize = 128 * 1024;
/// Size of each fresh stack segment used by deep expression evaluation.
const EVALUATOR_STACK_SEGMENT: usize = 2 * 1024 * 1024;

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
    /// Maximum active calls (function, closure and callback bodies) and
    /// maximum nesting of any single pattern or decoded value. Expression
    /// nesting inside a body is bounded by the parser and is not counted.
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

/// One bounded ordered relation page whose rows retain their value profiles
/// and format-3 owner contexts across an evaluator source scan.
#[derive(Clone, Debug)]
pub struct ContextValueRelationPage {
    pub rows: Vec<ContextValue>,
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
    /// Declared payload field names; empty for a payload-free variant.
    payload_fields: Vec<String>,
}

impl NominalVariant {
    /// Create an admitted enum variant with its stable ObjectId.
    #[must_use]
    pub fn new(variant_id: [u8; 16], name: impl Into<String>) -> Self {
        Self {
            variant_id: object_id_raw(variant_id),
            name: name.into(),
            payload_fields: Vec::new(),
        }
    }

    /// Declare the record payload fields used by `Enum.variant { ... }`.
    #[must_use]
    pub fn with_payload_fields(mut self, fields: Vec<String>) -> Self {
        self.payload_fields = fields;
        self
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
    currency: Option<CurrencyMetadata>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CurrencyMetadata {
    code: String,
    minor_digits: usize,
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
            currency: None,
        }
    }

    /// Associate this nominal identity with the stable Currency protocol metadata.
    #[must_use]
    pub fn with_currency(
        mut self,
        code: impl Into<String>,
        minor_digits: usize,
    ) -> Self {
        self.currency = Some(CurrencyMetadata {
            code: code.into(),
            minor_digits,
        });
        self
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

/// A closed `sys.<Enum>.<variant>` constant, unless a local binding shadows
/// the `sys` root. Enum constants are pure values, so no host is consulted.
fn system_enum_constant(expression: &Expr, scope: &Scope) -> Option<(String, String)> {
    let mut root = expression;
    while let Expr::Field { base, .. } = root {
        root = base;
    }
    let Expr::Name { text, .. } = root else {
        return None;
    };
    if text != "sys" || scope.0.contains_key("sys") {
        return None;
    }
    let mut segments = Vec::new();
    let mut current = expression;
    while let Expr::Field { base, name, .. } = current {
        segments.push(name.as_str());
        current = base;
    }
    segments.push("sys");
    segments.reverse();
    orna_semantic_v1::system_enum_constant(&segments)
}

/// Replaces every format-3 Blob reference in a raw REPL result with its
/// payload-free summary. Map entries are re-sorted by encoded key because a
/// summarised key may no longer sort where its reference did.
fn summarise_rov3_blobs(raw: Raw) -> Result<Raw, EvaluationError> {
    Ok(match raw {
        reference @ Raw::Tag(ROV3_BLOB_TAG, _) => rov3_blob_summary(&reference)?,
        Raw::Tag(tag, payload) => Raw::Tag(tag, Box::new(summarise_rov3_blobs(*payload)?)),
        Raw::Array(values) => Raw::Array(
            values
                .into_iter()
                .map(summarise_rov3_blobs)
                .collect::<Result<_, _>>()?,
        ),
        Raw::Map(entries) => {
            let mut entries = entries
                .into_iter()
                .map(|(key, value)| {
                    let key = summarise_rov3_blobs(key)?;
                    let value = summarise_rov3_blobs(value)?;
                    let encoded_key = CanonicalValue::new(key.clone())
                        .map_err(|_| error("ORNA-EVAL-VALUE"))?
                        .encode()
                        .map_err(|_| error("ORNA-EVAL-VALUE"))?;
                    Ok((encoded_key, key, value))
                })
                .collect::<Result<Vec<_>, EvaluationError>>()?;
            entries.sort_by(|(left, _, _), (right, _, _)| left.cmp(right));
            Raw::Map(
                entries
                    .into_iter()
                    .map(|(_, key, value)| (key, value))
                    .collect(),
            )
        }
        other => other,
    })
}

/// The payload-free REPL summary of one format-3 Blob reference: its
/// committed identity and annotation. The descriptor OID is not presented.
fn rov3_blob_summary(reference: &Raw) -> Result<Raw, EvaluationError> {
    let metadata =
        orna_value_v1::blob_metadata_from_raw(reference).map_err(|_| error("ORNA-EVAL-VALUE"))?;
    Ok(Raw::Map(vec![
        (Raw::Text("type".to_owned()), Raw::Text("Blob".to_owned())),
        (Raw::Text("length".to_owned()), Raw::Int(metadata.length().into())),
        (Raw::Text("sha256".to_owned()), Raw::Bytes(metadata.sha256().to_vec())),
        (
            Raw::Text("suffix".to_owned()),
            metadata
                .suffix()
                .map_or(Raw::Null, |suffix| Raw::Text(suffix.to_owned())),
        ),
        (
            Raw::Text("media_type".to_owned()),
            Raw::Text(metadata.media_type().to_owned()),
        ),
    ]))
}

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
    if let Some(currency) = &definition.currency
        && (currency.code.len() != 3
            || !currency.code.bytes().all(|byte| byte.is_ascii_uppercase())
            || currency.minor_digits > DEFAULT_INTEGER_DIGITS)
    {
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
        Value::List(values) => {
            for value in values {
                validate_admitted_nominals(value, definitions, limits)?;
            }
        }
        Value::Tuple(values) | Value::Stream { values, .. } => {
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

/// Admitted standard-library functions, shared by every admission that uses the
/// same pinned standard dependency. `functions` is keyed by qualified identity
/// (`std.<module>.<name>`); `aliases` maps module-internal import keys
/// (`std.<module>::<name>`) to qualified identities.
#[derive(Debug, Default)]
pub struct StandardLayer {
    functions: BTreeMap<String, PureFunction>,
    aliases: BTreeMap<String, String>,
}

impl StandardLayer {
    /// Builds a shared standard layer from verified qualified functions and
    /// module-internal import aliases.
    #[must_use]
    pub fn new(
        functions: BTreeMap<String, PureFunction>,
        aliases: BTreeMap<String, String>,
    ) -> Self {
        Self { functions, aliases }
    }
}

/// Explicitly admitted named functions; no host or module lookup is performed.
///
/// The table is layered: an optional `Arc`-shared standard layer, which is never
/// copied per admission, and an owned local layer for the admitted module.
/// Local bare-name import aliases resolve through [`Functions::alias`]. Local
/// functions shadow standard functions of the same key.
#[derive(Clone, Debug, Default)]
pub struct Functions {
    standard: Option<Arc<StandardLayer>>,
    local: BTreeMap<String, PureFunction>,
    local_aliases: BTreeMap<String, String>,
}

impl Functions {
    /// Creates an empty table with no standard layer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a table that shares `standard` without copying its bodies.
    #[must_use]
    pub fn with_standard(standard: Arc<StandardLayer>) -> Self {
        Self {
            standard: Some(standard),
            ..Self::default()
        }
    }

    /// Inserts a local function, replacing any local function of the same key.
    pub fn insert(&mut self, name: String, function: PureFunction) -> Option<PureFunction> {
        self.local.insert(name, function)
    }

    /// Inserts a bare-name import alias for the admitted module. Callers must
    /// insert user functions first and skip any alias that names one of them.
    pub fn insert_alias(&mut self, alias: String, target: String) {
        self.local_aliases.insert(alias, target);
    }

    /// Returns the target of a local bare-name alias or a standard module-internal
    /// alias key.
    #[must_use]
    pub fn alias(&self, key: &str) -> Option<&str> {
        self.local_aliases
            .get(key)
            .or_else(|| {
                self.standard
                    .as_deref()
                    .and_then(|layer| layer.aliases.get(key))
            })
            .map(String::as_str)
    }

    /// Returns the function for a qualified or local name. Aliases are not
    /// resolved here.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&PureFunction> {
        self.local.get(name).or_else(|| {
            self.standard
                .as_deref()
                .and_then(|layer| layer.functions.get(name))
        })
    }

    /// Reports whether a local or standard function has `name`.
    #[must_use]
    pub fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Returns the number of functions visible through this table.
    #[must_use]
    pub fn len(&self) -> usize {
        self.iter().count()
    }

    /// Returns the number of functions the admitted module owns. Shared standard
    /// functions are excluded so they never consume a caller's activation budget.
    #[must_use]
    pub fn own_len(&self) -> usize {
        self.local.len()
    }

    /// Reports whether no function is visible through this table.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Iterates every visible function. A local function shadows a standard
    /// function with the same key.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &PureFunction)> {
        let standard = self
            .standard
            .as_deref()
            .into_iter()
            .flat_map(|layer| layer.functions.iter())
            .filter(|(name, _)| !self.local.contains_key(*name));
        self.local.iter().chain(standard)
    }

    /// Iterates the names of every visible function.
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.iter().map(|(name, _)| name)
    }

    /// Iterates every visible function body.
    pub fn values(&self) -> impl Iterator<Item = &PureFunction> {
        self.iter().map(|(_, function)| function)
    }

    /// Iterates the admitted module's own function bodies for mutation. The
    /// shared standard layer is immutable and is never yielded.
    pub fn local_values_mut(&mut self) -> impl Iterator<Item = &mut PureFunction> {
        self.local.values_mut()
    }

    /// Returns the function a name denotes: an exact function key, or an import
    /// alias (local or module-internal) whose target is a function. Entry points
    /// and reachability walks accept either spelling.
    #[must_use]
    pub fn get_entry(&self, name: &str) -> Option<&PureFunction> {
        self.get(name)
            .or_else(|| self.alias(name).and_then(|target| self.get(target)))
    }

    /// Iterates every name that can denote a visible function: function keys
    /// and alias keys, both local and standard.
    pub fn visible_names(&self) -> impl Iterator<Item = &str> {
        let standard_aliases = self
            .standard
            .as_deref()
            .into_iter()
            .flat_map(|layer| layer.aliases.keys());
        self.keys()
            .chain(self.local_aliases.keys())
            .chain(standard_aliases)
            .map(String::as_str)
    }
}

impl<const N: usize> From<[(String, PureFunction); N]> for Functions {
    fn from(entries: [(String, PureFunction); N]) -> Self {
        Self {
            local: entries.into_iter().collect(),
            ..Self::default()
        }
    }
}

impl Functions {
    /// Iterates the admitted module's own function bodies mutably. Shared
    /// standard bodies are immutable and are never rewritten here.
    pub fn local_iter_mut(&mut self) -> impl Iterator<Item = (&String, &mut PureFunction)> {
        self.local.iter_mut()
    }

    /// Merges `other`'s own functions and aliases into this table; a same-key
    /// entry from `other` replaces this table's. A standard layer is adopted
    /// when this table has none. A second, distinct standard layer is refused
    /// before anything is merged, so a failed call leaves this table unchanged.
    pub fn extend(&mut self, other: Functions) -> Result<(), StandardLayerConflict> {
        if let (Some(layer), Some(other_layer)) = (&self.standard, &other.standard)
            && !Arc::ptr_eq(layer, other_layer)
        {
            return Err(StandardLayerConflict);
        }
        self.local.extend(other.local);
        self.local_aliases.extend(other.local_aliases);
        if self.standard.is_none() {
            self.standard = other.standard;
        }
        Ok(())
    }
}

/// Two distinct standard layers cannot be merged into one function table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StandardLayerConflict;

impl std::fmt::Display for StandardLayerConflict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("functions received a second, distinct standard layer")
    }
}

impl std::error::Error for StandardLayerConflict {}

impl FromIterator<(String, PureFunction)> for Functions {
    fn from_iter<I: IntoIterator<Item = (String, PureFunction)>>(entries: I) -> Self {
        Self {
            local: entries.into_iter().collect(),
            ..Self::default()
        }
    }
}

/// Internal key for an import resolved relative to a captured module body.
/// The separator cannot occur in a source-level Orna name.
pub fn module_function_alias_key(namespace: &str, name: &str) -> String {
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
        let mut diagnostic = Diagnostic::new(
            SafeText::new("ORNA-EVAL-ERROR").expect("static safe code"),
            DiagnosticSeverity::Error,
            SafeText::redacted(),
        )
        .expect("safe diagnostic code");
        for cause in value.causes() {
            diagnostic = diagnostic.with_cause(canonical_cause_diagnostic(&cause));
        }
        Self {
            diagnostic: Box::new(diagnostic.redacted()),
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

/// Diagnostic codes the evaluator itself emits. A nested canonical cause keeps
/// its code only when it is one of these; every other code, including a
/// user-authored `ORNA-` code, is redacted at the evaluator boundary.
const EVALUATOR_CAUSE_CODES: &[&str] = &[
    "ORNA-EVAL-AMBIGUOUS",
    "ORNA-EVAL-ARGUMENT",
    "ORNA-EVAL-BROWSER",
    "ORNA-EVAL-BROWSER-ENTRY",
    "ORNA-EVAL-CANCELLED",
    "ORNA-EVAL-DIVIDE-BY-ZERO",
    "ORNA-EVAL-ERROR",
    "ORNA-EVAL-FIELD",
    "ORNA-EVAL-IMMUTABLE-CAPTURE",
    "ORNA-EVAL-INDEX",
    "ORNA-EVAL-LIMIT",
    "ORNA-EVAL-NAME",
    "ORNA-EVAL-NO-MATCH",
    "ORNA-EVAL-PARSE",
    "ORNA-EVAL-RELATION-ONE-MULTIPLE",
    "ORNA-EVAL-RELATION-ONE-ZERO",
    "ORNA-EVAL-TIMEOUT",
    "ORNA-EVAL-TYPE",
    "ORNA-EVAL-UNSUPPORTED",
    "ORNA-EVAL-VALUE",
];

/// Projects a nested canonical cause to a code-only diagnostic. Only a code in
/// [`EVALUATOR_CAUSE_CODES`] is kept; every message and span stays redacted.
fn canonical_cause_diagnostic(cause: &CanonicalErrorValue) -> Diagnostic {
    let code = cause.code();
    let safe_code = if EVALUATOR_CAUSE_CODES.contains(&code) {
        SafeText::new(code).expect("catalogue code is safe text")
    } else {
        SafeText::redacted()
    };
    let mut diagnostic =
        Diagnostic::new(safe_code, DiagnosticSeverity::Error, SafeText::redacted())
            .expect("safe diagnostic code");
    for nested in cause.causes() {
        diagnostic = diagnostic.with_cause(canonical_cause_diagnostic(&nested));
    }
    diagnostic.redacted()
}

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

    /// The host catalogue that pins `sys.explain` inputs to the activation's
    /// snapshot. Without one, query and function explains report
    /// `sys.version.incompatible`.
    #[cfg(feature = "system-host-bindings")]
    fn explain_catalogue(&self) -> Option<&dyn explain_bindings::ExplainCatalogue> {
        None
    }

    /// The activation facts `sys.rt.info()` reports. Handlers without a host
    /// context leave `sys.rt.info()` reporting `sys.version.incompatible`.
    #[cfg(feature = "system-host-bindings")]
    fn sys_host_context(&self) -> Option<&SysHostAttachment> {
        None
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

    /// Resolves a bare table name read as a relation (`Note | count`) to the
    /// source name that [`EffectHandler::scan_relation_page`] serves. Returns
    /// `None` when the handler admits no table of that name, so the name keeps
    /// its ordinary resolution.
    fn admitted_relation_source(&self, _name: &str) -> Option<String> {
        None
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

    #[doc(hidden)]
    fn is_ovb2_binding(&self) -> bool {
        false
    }

    #[doc(hidden)]
    fn ovb2_binding(&mut self) -> Option<&mut dyn Ovb2EffectHandler> {
        None
    }
}

/// Executes native effects with values carried as [`ContextValue`]s.
///
/// OVB-2 remains the default profile. Context-aware named invocation can also
/// preserve ROV-3/SOV-3 owner contexts and ordered relation rows across this
/// boundary, while legacy [`EffectHandler`] callers stay canonical.
pub trait Ovb2EffectHandler {
    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[ContextValue],
    ) -> Result<Option<ContextValue>, EvaluationError>;

    /// Handle an effect while sharing the activation's step budget.
    fn handle_with_budget(
        &mut self,
        callee: &Expr,
        arguments: &[ContextValue],
        _budget: &mut StepBudget,
    ) -> Result<Option<ContextValue>, EvaluationError> {
        self.handle(callee, arguments)
    }

    /// Handle a native operation resolved by its admitted system identity.
    fn handle_registered_with_budget(
        &mut self,
        _operation: &str,
        callee: &Expr,
        arguments: &[ContextValue],
        budget: &mut StepBudget,
    ) -> Result<Option<ContextValue>, EvaluationError> {
        self.handle_with_budget(callee, arguments, budget)
    }

    /// Selects the ContextValue profile used by context-aware named invocation.
    /// Existing OVB-2 handlers keep their original default profile.
    fn value_format(&self) -> ValueFormat {
        ValueFormat::Ovb2
    }

    /// Supplies the capability context for ROV-3/SOV-3 values.
    fn value_context(&self) -> Option<&Format3Context> {
        None
    }

    /// Whether this binding accepts formats beyond OVB-2.
    fn accepts_context_values(&self) -> bool {
        false
    }

    /// Handle a registered operation while retaining the live cancellation
    /// request for host operations that can stop promptly.
    fn handle_registered_with_cancellation_and_budget(
        &mut self,
        operation: &str,
        callee: &Expr,
        arguments: &[ContextValue],
        budget: &mut StepBudget,
        _cancellation: Option<&CancellationToken>,
    ) -> Result<Option<ContextValue>, EvaluationError> {
        self.handle_registered_with_budget(operation, callee, arguments, budget)
    }

    /// Supplies ordered relation rows without flattening contextual values to
    /// canonical OVB-1. `None` falls back to the canonical page hook.
    fn scan_context_relation_page_scoped(
        &mut self,
        _source: &str,
        _scope: RelationReadScope,
        _after: Option<&[u8]>,
        _limit: usize,
        _budget: &mut StepBudget,
    ) -> Result<Option<ContextValueRelationPage>, EvaluationError> {
        Ok(None)
    }

    /// The host catalogue that pins `sys.explain` inputs to the activation's
    /// snapshot. Without one, query and function explains report
    /// `sys.version.incompatible`.
    #[cfg(feature = "system-host-bindings")]
    fn explain_catalogue(&self) -> Option<&dyn explain_bindings::ExplainCatalogue> {
        None
    }

    /// The activation facts `sys.rt.info()` reports. Handlers without a host
    /// context leave `sys.rt.info()` reporting `sys.version.incompatible`.
    #[cfg(feature = "system-host-bindings")]
    fn sys_host_context(&self) -> Option<&SysHostAttachment> {
        None
    }
}

struct Ovb2EffectHandlerAdapter<'a> {
    handler: &'a mut dyn Ovb2EffectHandler,
}

impl EffectHandler for Ovb2EffectHandlerAdapter<'_> {
    fn handle(
        &mut self,
        _callee: &Expr,
        _arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        Err(error("ORNA-EVAL-UNSUPPORTED"))
    }

    fn is_ovb2_binding(&self) -> bool {
        true
    }

    fn ovb2_binding(&mut self) -> Option<&mut dyn Ovb2EffectHandler> {
        Some(self.handler)
    }

    #[cfg(feature = "system-host-bindings")]
    fn explain_catalogue(&self) -> Option<&dyn explain_bindings::ExplainCatalogue> {
        self.handler.explain_catalogue()
    }

    #[cfg(feature = "system-host-bindings")]
    fn sys_host_context(&self) -> Option<&SysHostAttachment> {
        self.handler.sys_host_context()
    }
}

/// Effect and relation boundary for named invocation with preserved
/// ContextValues. Arguments retain their profile and format-3 capability
/// context; results may establish the owner context for subsequent effects.
pub trait ContextValueEffectHandler: EffectHandler {
    fn handle_context(
        &mut self,
        callee: &Expr,
        arguments: &[ContextValue],
    ) -> Result<Option<ContextValue>, EvaluationError>;

    fn handle_context_with_budget(
        &mut self,
        callee: &Expr,
        arguments: &[ContextValue],
        _budget: &mut StepBudget,
    ) -> Result<Option<ContextValue>, EvaluationError> {
        self.handle_context(callee, arguments)
    }

    fn handle_context_registered_with_budget(
        &mut self,
        _operation: &str,
        callee: &Expr,
        arguments: &[ContextValue],
        budget: &mut StepBudget,
    ) -> Result<Option<ContextValue>, EvaluationError> {
        self.handle_context_with_budget(callee, arguments, budget)
    }

    fn handle_context_registered_with_cancellation_and_budget(
        &mut self,
        operation: &str,
        callee: &Expr,
        arguments: &[ContextValue],
        budget: &mut StepBudget,
        _cancellation: Option<&CancellationToken>,
    ) -> Result<Option<ContextValue>, EvaluationError> {
        self.handle_context_registered_with_budget(operation, callee, arguments, budget)
    }
    /// Supplies rows for a context-preserving indexed source scan. Returning
    /// `None` keeps the existing canonical `EffectHandler` scan path.
    fn scan_context_relation_page_scoped(
        &mut self,
        _source: &str,
        _scope: RelationReadScope,
        _after: Option<&[u8]>,
        _limit: usize,
        _budget: &mut StepBudget,
    ) -> Result<Option<ContextValueRelationPage>, EvaluationError> {
        Ok(None)
    }
}

struct ContextValueEffectHandlerAdapter<'a> {
    handler: &'a mut dyn ContextValueEffectHandler,
    format: ValueFormat,
    context: Option<Format3Context>,
    source_format: ValueFormat,
    source_context: Option<Format3Context>,
}

impl ContextValueEffectHandlerAdapter<'_> {
    fn record_result_profile(&mut self, result: &Option<ContextValue>) {
        if let Some(value) = result {
            self.format = value.format();
            self.context = value.context().cloned();
        }
    }
    fn profile(&self) -> (ValueFormat, Option<&Format3Context>) {
        if matches!(self.format, ValueFormat::Rov3 | ValueFormat::Sov3) && self.context.is_some()
        {
            (self.format, self.context.as_ref())
        } else if matches!(self.source_format, ValueFormat::Rov3 | ValueFormat::Sov3) {
            (self.source_format, self.source_context.as_ref())
        } else {
            (self.format, self.context.as_ref())
        }
    }
}

impl Ovb2EffectHandler for ContextValueEffectHandlerAdapter<'_> {
    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[ContextValue],
    ) -> Result<Option<ContextValue>, EvaluationError> {
        let result = self.handler.handle_context(callee, arguments)?;
        self.record_result_profile(&result);
        Ok(result)
    }

    fn handle_with_budget(
        &mut self,
        callee: &Expr,
        arguments: &[ContextValue],
        budget: &mut StepBudget,
    ) -> Result<Option<ContextValue>, EvaluationError> {
        let result = self
            .handler
            .handle_context_with_budget(callee, arguments, budget)?;
        self.record_result_profile(&result);
        Ok(result)
    }

    fn handle_registered_with_budget(
        &mut self,
        operation: &str,
        callee: &Expr,
        arguments: &[ContextValue],
        budget: &mut StepBudget,
    ) -> Result<Option<ContextValue>, EvaluationError> {
        let result = self
            .handler
            .handle_context_registered_with_budget(operation, callee, arguments, budget)?;
        self.record_result_profile(&result);
        Ok(result)
    }

    fn handle_registered_with_cancellation_and_budget(
        &mut self,
        operation: &str,
        callee: &Expr,
        arguments: &[ContextValue],
        budget: &mut StepBudget,
        cancellation: Option<&CancellationToken>,
    ) -> Result<Option<ContextValue>, EvaluationError> {
        let result = self
            .handler
            .handle_context_registered_with_cancellation_and_budget(
                operation,
                callee,
                arguments,
                budget,
                cancellation,
            )?;
        self.record_result_profile(&result);
        Ok(result)
    }

    fn scan_context_relation_page_scoped(
        &mut self,
        source: &str,
        scope: RelationReadScope,
        after: Option<&[u8]>,
        limit: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<ContextValueRelationPage>, EvaluationError> {
        let page = self
            .handler
            .scan_context_relation_page_scoped(source, scope, after, limit, budget)?;
        if let Some(row) = page.as_ref().and_then(|page| page.rows.first())
            && matches!(row.format(), ValueFormat::Rov3 | ValueFormat::Sov3)
            && let Some(context) = row.context()
        {
            self.format = row.format();
            self.context = Some(context.clone());
            self.source_format = row.format();
            self.source_context = Some(context.clone());
        }
        Ok(page)
    }

    fn value_format(&self) -> ValueFormat {
        self.profile().0
    }

    fn value_context(&self) -> Option<&Format3Context> {
        self.profile().1
    }

    fn accepts_context_values(&self) -> bool {
        true
    }

    #[cfg(feature = "system-host-bindings")]
    fn explain_catalogue(&self) -> Option<&dyn explain_bindings::ExplainCatalogue> {
        self.handler.explain_catalogue()
    }

    #[cfg(feature = "system-host-bindings")]
    fn sys_host_context(&self) -> Option<&SysHostAttachment> {
        self.handler.sys_host_context()
    }
}

impl EffectHandler for ContextValueEffectHandlerAdapter<'_> {
    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        EffectHandler::handle(self.handler, callee, arguments)
    }

    fn handle_with_budget(
        &mut self,
        callee: &Expr,
        arguments: &[CanonicalValue],
        budget: &mut StepBudget,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        EffectHandler::handle_with_budget(self.handler, callee, arguments, budget)
    }

    #[cfg(feature = "system-host-bindings")]
    fn explain_catalogue(&self) -> Option<&dyn explain_bindings::ExplainCatalogue> {
        self.handler.explain_catalogue()
    }

    #[cfg(feature = "system-host-bindings")]
    fn sys_host_context(&self) -> Option<&SysHostAttachment> {
        self.handler.sys_host_context()
    }

    fn handle_registered_with_budget(
        &mut self,
        operation: &str,
        callee: &Expr,
        arguments: &[CanonicalValue],
        budget: &mut StepBudget,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        EffectHandler::handle_registered_with_budget(
            self.handler,
            operation,
            callee,
            arguments,
            budget,
        )
    }

    fn handle_registered_with_cancellation_and_budget(
        &mut self,
        operation: &str,
        callee: &Expr,
        arguments: &[CanonicalValue],
        budget: &mut StepBudget,
        cancellation: Option<&CancellationToken>,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        EffectHandler::handle_registered_with_cancellation_and_budget(
            self.handler,
            operation,
            callee,
            arguments,
            budget,
            cancellation,
        )
    }

    fn fork_task_child(&mut self, child_index: usize) -> Option<Box<dyn EffectHandler + Send>> {
        EffectHandler::fork_task_child(self.handler, child_index)
    }

    fn resolve_reference(
        &mut self,
        reference: &CanonicalValue,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        EffectHandler::resolve_reference(self.handler, reference)
    }

    fn resolve_reference_with_budget(
        &mut self,
        reference: &CanonicalValue,
        budget: &mut StepBudget,
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        EffectHandler::resolve_reference_with_budget(self.handler, reference, budget)
    }

    fn validate_relation_source(&mut self, source: &str) -> Result<(), EvaluationError> {
        EffectHandler::validate_relation_source(self.handler, source)
    }

    fn admitted_relation_source(&self, name: &str) -> Option<String> {
        EffectHandler::admitted_relation_source(&*self.handler, name)
    }

    fn scan_relation_page(
        &mut self,
        source: &str,
        after: Option<&[u8]>,
        limit: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<RelationPage>, EvaluationError> {
        EffectHandler::scan_relation_page(self.handler, source, after, limit, budget)
    }

    fn scan_relation_page_scoped(
        &mut self,
        source: &str,
        scope: RelationReadScope,
        after: Option<&[u8]>,
        limit: usize,
        budget: &mut StepBudget,
    ) -> Result<Option<RelationPage>, EvaluationError> {
        EffectHandler::scan_relation_page_scoped(
            self.handler,
            source,
            scope,
            after,
            limit,
            budget,
        )
    }

    fn validate_stream_source(
        &mut self,
        source: &str,
        identity: &[u8; 32],
        budget: &mut StepBudget,
        cancellation: Option<&CancellationToken>,
    ) -> Result<bool, EvaluationError> {
        EffectHandler::validate_stream_source(
            self.handler,
            source,
            identity,
            budget,
            cancellation,
        )
    }

    fn poll_stream_sources(
        &mut self,
        sources: &[StreamSourceCursor],
        max_events: usize,
        budget: &mut StepBudget,
        cancellation: Option<&CancellationToken>,
    ) -> Result<StreamPage, EvaluationError> {
        EffectHandler::poll_stream_sources(
            self.handler,
            sources,
            max_events,
            budget,
            cancellation,
        )
    }

    fn begin_stream_delivery(
        &mut self,
        checkpoints: &[StreamCheckpoint],
        budget: &mut StepBudget,
        cancellation: Option<&CancellationToken>,
    ) -> Result<(), EvaluationError> {
        EffectHandler::begin_stream_delivery(self.handler, checkpoints, budget, cancellation)
    }

    fn commit_stream_delivery(
        &mut self,
        checkpoints: &[StreamCheckpoint],
        budget: &mut StepBudget,
        cancellation: Option<&CancellationToken>,
    ) -> Result<(), EvaluationError> {
        EffectHandler::commit_stream_delivery(self.handler, checkpoints, budget, cancellation)
    }

    fn rollback_stream_delivery(
        &mut self,
        checkpoints: &[StreamCheckpoint],
        budget: &mut StepBudget,
        cancellation: Option<&CancellationToken>,
    ) -> Result<(), EvaluationError> {
        EffectHandler::rollback_stream_delivery(self.handler, checkpoints, budget, cancellation)
    }

    fn is_ovb2_binding(&self) -> bool {
        true
    }

    fn ovb2_binding(&mut self) -> Option<&mut dyn Ovb2EffectHandler> {
        Some(self)
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

/// Builds planner metadata for a typed relation query without executing its source scan.
///
/// Evaluation proceeds only far enough to construct the relation plan. Source reads,
/// callbacks, and terminal operators that materialize rows are not run.
#[must_use]
pub fn planner_metadata(
    source: &str,
    environment: &Environment,
    limits: Limits,
) -> Result<RelationPlanMetadata, EvaluationError> {
    check_limits(source, limits)?;
    let parsed = parse_expression(source);
    if !parsed.is_ok() {
        return Err(error("ORNA-EVAL-PARSE"));
    }
    if !planner_metadata_relation_expression(&parsed.value) {
        return Err(error("ORNA-EVAL-UNSUPPORTED"));
    }
    validate_limits(limits)?;
    let functions = Functions::new();
    let mut context = Context {
        limits,
        steps: 0,
        calls: 0,
        functions: &functions,
        aliases: None,
        session_functions: None,
        repl_bindings: false,
        restrict_function_names: false,
        reject_unhandled_field_calls: false,
        effects: None,
        namespace: None,
        transfer: None,
        materializations: Vec::new(),
        materialization_clock_unavailable: false,
        reuse: CallReuse::default(),
        sys_host: None,
        cancellation: None,
    };
    let mut scope = Scope::from_environment(environment, &mut context)?;
    let value = context.evaluate(&parsed.value, &mut scope, 0)?;
    if context.transfer.is_some() {
        return Err(error("ORNA-EVAL-UNSUPPORTED"));
    }
    match value {
        Value::Relation(plan) => Ok(plan.metadata()),
        _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
    }
}
fn planner_metadata_relation_expression(expression: &Expr) -> bool {
    match expression {
        Expr::Group { inner, .. } => planner_metadata_relation_expression(inner),
        Expr::Binary { lhs, op, rhs, .. } if op == "|" => {
            planner_metadata_relation_expression(lhs) && planner_metadata_lazy_stage(rhs)
        }
        _ => system_relation_source(expression).is_some(),
    }
}

fn planner_metadata_lazy_stage(expression: &Expr) -> bool {
    let (callee, arguments) = match expression {
        Expr::Call { callee, arguments, .. } | Expr::GenericCall { callee, arguments, .. } => {
            (callee, arguments)
        }
        Expr::Group { inner, .. } => return planner_metadata_lazy_stage(inner),
        _ => return false,
    };
    let Some(name) = function_name(callee) else {
        return false;
    };
    let operation = name
        .strip_prefix("std.collection.")
        .or_else(|| name.strip_prefix("std.query."))
        .unwrap_or(&name);
    if operation == "union" {
        return arguments
            .iter()
            .all(|argument| planner_metadata_relation_expression(&argument.value));
    }
    matches!(
        operation,
        "filter"
            | "map"
            | "project"
            | "flat_map"
            | "sort_by"
            | "bucket_by"
            | "window"
            | "distinct"
            | "unique"
            | "pairs"
            | "take"
            | "drop"
    )
}
/// Evaluate one expression using OVB-2 context values at the boundary.
///
/// Every environment value must carry the OVB-2 profile. Annotated Blobs are
/// retained as typed values while evaluating and are returned with their MIME
/// type and suffix intact. This is an additive boundary; the OVB-1
/// [`Environment`] and [`evaluate_expression`] APIs remain unchanged.
pub fn evaluate_expression_ovb2(
    source: &str,
    environment: &BTreeMap<String, ContextValue>,
    limits: Limits,
) -> Result<ContextValue, EvaluationError> {
    evaluate_expression_ovb2_with_optional_effects(source, environment, limits, None, None)
}

/// Evaluate one expression using OVB-2 values and a profile-aware native
/// effect binding. Effect arguments and results retain the OVB-2 profile,
/// including MIME-1 annotations on Blob values.
pub fn evaluate_expression_ovb2_with_effects(
    source: &str,
    environment: &BTreeMap<String, ContextValue>,
    limits: Limits,
    effects: &mut dyn Ovb2EffectHandler,
) -> Result<ContextValue, EvaluationError> {
    evaluate_expression_ovb2_with_optional_effects(source, environment, limits, Some(effects), None)
}

/// OVB-2 expression evaluation with native effects and an optional live
/// cancellation request.
pub fn evaluate_expression_ovb2_with_effects_and_cancellation(
    source: &str,
    environment: &BTreeMap<String, ContextValue>,
    limits: Limits,
    effects: &mut dyn Ovb2EffectHandler,
    cancellation: Option<&CancellationToken>,
) -> Result<ContextValue, EvaluationError> {
    evaluate_expression_ovb2_with_optional_effects(
        source,
        environment,
        limits,
        Some(effects),
        cancellation,
    )
}

fn evaluate_expression_ovb2_with_optional_effects<'functions, 'effects>(
    source: &str,
    environment: &BTreeMap<String, ContextValue>,
    limits: Limits,
    effects: Option<&'effects mut dyn Ovb2EffectHandler>,
    cancellation: Option<&'functions CancellationToken>,
) -> Result<ContextValue, EvaluationError> {
    check_limits(source, limits)?;
    let parsed = parse_expression(source);
    if !parsed.is_ok() {
        return Err(error("ORNA-EVAL-PARSE"));
    }
    validate_limits(limits)?;
    let functions = Functions::new();
    let mut effects_adapter = effects.map(|handler| Ovb2EffectHandlerAdapter { handler });
    let effects = effects_adapter
        .as_mut()
        .map(|adapter| adapter as &mut dyn EffectHandler);
    let mut context = Context {
        limits,
        steps: 0,
        calls: 0,
        functions: &functions,
        aliases: None,
        session_functions: None,
        repl_bindings: false,
        restrict_function_names: false,
        reject_unhandled_field_calls: false,
        effects,
        namespace: None,
        transfer: None,
        materializations: Vec::new(),
        materialization_clock_unavailable: false,
        reuse: CallReuse::default(),
        sys_host: None,
        cancellation,
    };
    let mut scope = Scope::from_ovb2_environment(environment, &mut context)?;
    let value = context.evaluate(&parsed.value, &mut scope, 0)?;
    if context.transfer.is_some() {
        return Err(error("ORNA-EVAL-UNSUPPORTED"));
    }
    // A Blob that an effect returned in its format-3 context (a lazy capture)
    // keeps that context; every other result is OVB-2.
    Context::contextual_value(value, ValueFormat::Ovb2, None)
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
        calls: 0,
        functions,
        aliases: None,
        session_functions: None,
        repl_bindings: false,
        restrict_function_names: false,
        reject_unhandled_field_calls: false,
        effects: Some(effects),
        namespace: None,
        transfer: None,
        materializations: Vec::new(),
        materialization_clock_unavailable: false,
        reuse: CallReuse::default(),
        sys_host: None,
        cancellation,
    };
    context.items(functions.own_len())?;
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
        ReplInput::ExpressionStatement(_) | ReplInput::Item(_) => {
            Err(error("ORNA-EVAL-UNSUPPORTED"))
        }
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
        calls: 0,
        functions,
        aliases: None,
        session_functions: None,
        repl_bindings: false,
        restrict_function_names: false,
        reject_unhandled_field_calls: false,
        effects: None,
        namespace: None,
        transfer: None,
        materializations: Vec::new(),
        materialization_clock_unavailable: false,
        reuse: CallReuse::default(),
        sys_host: None,
        cancellation: None,
    };
    let result = (|| {
        context.items(functions.own_len())?;
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
        calls: 0,
        functions: &functions,
        aliases: None,
        session_functions: None,
        repl_bindings: false,
        restrict_function_names: false,
        reject_unhandled_field_calls: false,
        effects: None,
        namespace: None,
        transfer: None,
        materializations: Vec::new(),
        materialization_clock_unavailable: false,
        reuse: CallReuse::default(),
        sys_host: None,
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
    invoke_named_with_host(
        name,
        functions,
        arguments,
        nominal_definitions,
        limits,
        None,
    )
}

/// Invoke an admitted named function with the activation facts a host
/// attached for `sys.rt.info()` (SYS-022..030). The carrier is read only by
/// the sys singleton path and grants no other capability.
#[cfg(feature = "system-host-bindings")]
pub fn invoke_named_with_sys_host_context(
    name: &str,
    functions: &Functions,
    arguments: &Environment,
    nominal_definitions: &NominalDefinitions,
    limits: Limits,
    sys_host: &SysHostContext,
) -> Result<CanonicalValue, EvaluationError> {
    let attachment = SysHostAttachment::new(sys_host.clone());
    invoke_named_with_host(
        name,
        functions,
        arguments,
        nominal_definitions,
        limits,
        Some(&attachment),
    )
}

fn invoke_named_with_host(
    name: &str,
    functions: &Functions,
    arguments: &Environment,
    nominal_definitions: &NominalDefinitions,
    limits: Limits,
    sys_host: ActivationSysHost<'_>,
) -> Result<CanonicalValue, EvaluationError> {
    validate_limits(limits)?;
    let mut context = Context {
        limits,
        steps: 0,
        calls: 0,
        functions,
        aliases: None,
        session_functions: None,
        repl_bindings: false,
        restrict_function_names: false,
        reject_unhandled_field_calls: false,
        effects: None,
        namespace: function_namespace(name),
        transfer: None,
        materializations: Vec::new(),
        materialization_clock_unavailable: false,
        reuse: CallReuse::default(),
        sys_host,
        cancellation: None,
    };
    context.items(functions.own_len())?;
    let function = functions.get_entry(name).ok_or_else(|| error("ORNA-EVAL-NAME"))?;
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
    invoke_named_with_effects_nominals_and_budget(
        name,
        functions,
        arguments,
        &NominalDefinitions::new(),
        limits,
        effects,
        budget,
    )
}

/// Invokes an admitted function with declaration-owned nominal construction
/// plans while consuming a caller-owned activation budget. The plans are
/// retained by the initial lexical scope and reach nested named calls and
/// closures, as for [`invoke_named_with_nominals`].
pub fn invoke_named_with_effects_nominals_and_budget(
    name: &str,
    functions: &Functions,
    arguments: &Environment,
    nominal_definitions: &NominalDefinitions,
    limits: Limits,
    effects: &mut dyn EffectHandler,
    budget: &mut StepBudget,
) -> Result<CanonicalValue, EvaluationError> {
    invoke_named_with_effects_and_reuse(
        name,
        functions,
        arguments,
        nominal_definitions,
        limits,
        effects,
        budget,
    )
    .map(|(value, _)| value)
}

/// Invokes an admitted function with a fresh budget and reports the
/// activation-local call reuse decisions (ORNA-CALL-004). Reuse never changes
/// the returned value.
pub fn invoke_named_with_effects_and_reuse_report(
    name: &str,
    functions: &Functions,
    arguments: &Environment,
    limits: Limits,
    effects: &mut dyn EffectHandler,
) -> Result<(CanonicalValue, CallReuseStats), EvaluationError> {
    let mut budget = StepBudget::new(limits.max_steps);
    invoke_named_with_effects_and_reuse(
        name,
        functions,
        arguments,
        &NominalDefinitions::new(),
        limits,
        effects,
        &mut budget,
    )
}

fn invoke_named_with_effects_and_reuse(
    name: &str,
    functions: &Functions,
    arguments: &Environment,
    nominal_definitions: &NominalDefinitions,
    limits: Limits,
    effects: &mut dyn EffectHandler,
    budget: &mut StepBudget,
) -> Result<(CanonicalValue, CallReuseStats), EvaluationError> {
    validate_limits(limits)?;
    let mut context = Context {
        limits,
        steps: limits.max_steps.saturating_sub(budget.remaining()),
        calls: 0,
        functions,
        aliases: None,
        session_functions: None,
        repl_bindings: false,
        restrict_function_names: false,
        reject_unhandled_field_calls: false,
        effects: Some(effects),
        namespace: function_namespace(name),
        transfer: None,
        materializations: Vec::new(),
        materialization_clock_unavailable: false,
        cancellation: None,
        reuse: CallReuse::default(),
        sys_host: None,
    };
    let result = (|| {
        context.items(functions.own_len())?;
        let function = functions.get_entry(name).ok_or_else(|| error("ORNA-EVAL-NAME"))?;
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
    })();
    let stats = context.reuse.stats;
    *budget = StepBudget::new(limits.max_steps.saturating_sub(context.steps));
    result.map(|value| (value, stats))
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

/// Invokes an admitted function while consuming a caller-owned activation budget.
/// ContextValue inputs, source calls, host effects, and the result retain
/// format-3 capability contexts across nested evaluation.
pub fn invoke_named_with_context_effects(
    name: &str,
    functions: &Functions,
    arguments: &BTreeMap<String, ContextValue>,
    limits: Limits,
    effects: &mut dyn ContextValueEffectHandler,
    budget: &mut StepBudget,
) -> Result<ContextValue, EvaluationError> {
    validate_limits(limits)?;
    let format = arguments
        .values()
        .find(|value| matches!(value.format(), ValueFormat::Rov3 | ValueFormat::Sov3))
        .or_else(|| arguments.values().find(|value| value.format() == ValueFormat::Ovb2))
        .or_else(|| arguments.values().next())
        .map_or(ValueFormat::Ovb1, ContextValue::format);
    let value_context = arguments
        .values()
        .find_map(ContextValue::context)
        .cloned();
    if matches!(format, ValueFormat::Rov3 | ValueFormat::Sov3) && value_context.is_none() {
        return Err(error("ORNA-EVAL-VALUE"));
    }
    let mut profiled_effects = ContextValueEffectHandlerAdapter {
        handler: effects,
        format,
        context: value_context.clone(),
        source_format: format,
        source_context: value_context.clone(),
    };
    let (result, steps) = {
        let mut context = Context {
            limits,
            steps: limits.max_steps.saturating_sub(budget.remaining()),
            calls: 0,
            functions,
            aliases: None,
            session_functions: None,
            repl_bindings: false,
            restrict_function_names: false,
            reject_unhandled_field_calls: false,
            effects: Some(&mut profiled_effects),
            namespace: function_namespace(name),
            transfer: None,
            materializations: Vec::new(),
            materialization_clock_unavailable: false,
            reuse: CallReuse::default(),
            sys_host: None,
            cancellation: None,
        };
        let result = (|| {
            context.items(functions.own_len())?;
            let function = functions.get_entry(name).ok_or_else(|| error("ORNA-EVAL-NAME"))?;
            let supplied = supplied_context_arguments(arguments, &mut context)?;
            let captured = Scope::from_environment(&function.environment, &mut context)?;
            invoke_pure(
                &mut context,
                &function.parameters,
                &function.body,
                captured,
                supplied,
                0,
            )
        })();
        let steps = context.steps;
        drop(context);
        let result = result.and_then(|value| {
            let (result_format, result_context) = profiled_effects.profile();
            Context::contextual_value(value, result_format, result_context)
        });
        (result, steps)
    };
    *budget = StepBudget::new(limits.max_steps.saturating_sub(steps));
    result
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

fn supplied_context_arguments(
    arguments: &BTreeMap<String, ContextValue>,
    context: &mut Context,
) -> Result<BTreeMap<ArgumentKey, Value>, EvaluationError> {
    Ok(Scope::from_context_environment(arguments, context)?
        .0
        .into_iter()
        .map(|(name, value)| (ArgumentKey::Name(name), value))
        .collect())
}

/// Runs one source activation. Every activation counts against
/// `Limits::max_depth`; exceeding it fails with the explicit `ORNA-EVAL-LIMIT`
/// resource code (ORNA-CLOSURE-004), never with a partial result.
fn invoke_pure(
    context: &mut Context,
    parameters: &[Parameter],
    body: &Expr,
    scope: Scope,
    supplied: BTreeMap<ArgumentKey, Value>,
    depth: usize,
) -> Result<Value, EvaluationError> {
    context.enter_call()?;
    let result = bind_and_evaluate(context, parameters, body, scope, supplied, depth);
    context.calls -= 1;
    result
}

fn bind_and_evaluate(
    context: &mut Context,
    parameters: &[Parameter],
    body: &Expr,
    mut scope: Scope,
    supplied: BTreeMap<ArgumentKey, Value>,
    depth: usize,
) -> Result<Value, EvaluationError> {
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
        if !bind(&parameter.pattern, value, &mut scope, context, 1)? {
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

/// Equality for `==` and `!=`: Blob values are compared by their annotation and
/// exact content, not by OVB-2 or format-3 representation. Lists, tuples and
/// options are compared element-wise so that a Blob nested inside them is
/// decided the same way. Every other pair keeps ordinary structural equality.
fn blob_aware_eq(left: &Value, right: &Value) -> Result<bool, EvaluationError> {
    match (left, right) {
        (Value::List(left), Value::List(right)) => {
            blob_aware_seq_eq(left.as_slice(), right.as_slice())
        }
        (Value::Tuple(left), Value::Tuple(right)) => blob_aware_seq_eq(left, right),
        (Value::Option(left), Value::Option(right)) => match (left.as_deref(), right.as_deref()) {
            (None, None) => Ok(true),
            (Some(left), Some(right)) => blob_aware_eq(left, right),
            _ => Ok(false),
        },
        (Value::Record(left), Value::Record(right)) => {
            if left.len() != right.len() {
                return Ok(false);
            }
            for ((left_key, left), (right_key, right)) in left.iter().zip(right) {
                if left_key != right_key || !blob_aware_eq(left, right)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (
            Value::NominalRecord {
                type_id: left_type,
                fields: left_fields,
            },
            Value::NominalRecord {
                type_id: right_type,
                fields: right_fields,
            },
        ) => {
            if left_type != right_type || left_fields.len() != right_fields.len() {
                return Ok(false);
            }
            for ((left_key, left), (right_key, right)) in left_fields.iter().zip(right_fields) {
                if left_key != right_key || !blob_aware_eq(left, right)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (
            Value::Enum {
                type_id: left_type,
                variant_id: left_variant,
                payload: left_payload,
            },
            Value::Enum {
                type_id: right_type,
                variant_id: right_variant,
                payload: right_payload,
            },
        ) => {
            if left_type != right_type || left_variant != right_variant {
                return Ok(false);
            }
            match (left_payload.as_deref(), right_payload.as_deref()) {
                (None, None) => Ok(true),
                (Some(left), Some(right)) => blob_aware_eq(left, right),
                _ => Ok(false),
            }
        }
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
            if left_inclusive != right_inclusive {
                return Ok(false);
            }
            Ok(optional_blob_aware_eq(left_lower.as_deref(), right_lower.as_deref())?
                && optional_blob_aware_eq(left_upper.as_deref(), right_upper.as_deref())?)
        }
        (
            Value::Stream {
                values: left_values,
                source_label: left_label,
                source_digest: left_digest,
                position: left_position,
                provider: left_provider,
            },
            Value::Stream {
                values: right_values,
                source_label: right_label,
                source_digest: right_digest,
                position: right_position,
                provider: right_provider,
            },
        ) => {
            if left_label != right_label
                || left_digest != right_digest
                || left_position != right_position
                || left_provider != right_provider
            {
                return Ok(false);
            }
            blob_aware_seq_eq(left_values, right_values)
        }
        (
            Value::AnnotatedBlob(_) | Value::ContextualBlob(_),
            Value::AnnotatedBlob(_) | Value::ContextualBlob(_),
        ) => {
            let left = annotated_blob(left)?;
            let right = annotated_blob(right)?;
            left.value_eq(&right).map_err(blob_equality_error)
        }
        _ => Ok(left == right),
    }
}

fn blob_aware_seq_eq(left: &[Value], right: &[Value]) -> Result<bool, EvaluationError> {
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left.iter().zip(right) {
        if !blob_aware_eq(left, right)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn optional_blob_aware_eq(
    left: Option<&Value>,
    right: Option<&Value>,
) -> Result<bool, EvaluationError> {
    match (left, right) {
        (None, None) => Ok(true),
        (Some(left), Some(right)) => blob_aware_eq(left, right),
        _ => Ok(false),
    }
}

/// Reads an annotated Blob value into its exact Blob, whichever representation
/// carries it. An OVB-2 Blob is validated from its raw wire value first.
fn annotated_blob(value: &Value) -> Result<orna_value_v1::Blob, EvaluationError> {
    let context_value = match value {
        Value::AnnotatedBlob(raw) => ContextValue::new(ValueFormat::Ovb2, raw.clone()),
        Value::ContextualBlob(value) => Ok(value.clone()),
        _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
    }
    .map_err(blob_equality_error)?;
    context_value.blob().map_err(blob_equality_error)
}

/// Maps Blob comparison failures to the portable codes the Blob contracts name.
fn blob_equality_error(failure: ValueError) -> EvaluationError {
    match failure {
        ValueError::ContentUnavailable => error("sys.blob.unavailable"),
        ValueError::ContentDigestMismatch => error("sys.blob.corrupt"),
        ValueError::InvalidContext => error("sys.blob.reference_context"),
        _ => error("ORNA-EVAL-VALUE"),
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
    /// An OVB-2 annotated Blob retained as its already validated wire value,
    /// independently from legacy byte strings.
    AnnotatedBlob(Raw),
    /// A Blob whose original value profile and format-3 capability pin remain live.
    ContextualBlob(ContextValue),
    Date(String),
    Uuid([u8; 16]),
    /// A nominal `Digest` (tag 60025): the raw-content SHA-256 commitment.
    Digest([u8; 32]),
    /// OVB's complete stored identity is retained, including its snapshot pin.
    Reference(CanonicalValue),
    /// A stored table reference (tag 60021): database identity, table and key.
    /// It embeds no row copy or snapshot; equality and ordering are by identity.
    TableReference(CanonicalValue),
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
    /// Shared immutable elements; clones and appends at the end of the
    /// longest view do not copy existing elements.
    List(List),
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
        let (exponent, left, right) = self.aligned_with(other)?;
        Self::new(left + right, exponent)
    }
    /// Exact truncated remainder, matching Int `%`: the result has the
    /// dividend's sign and a smaller magnitude than the divisor (ORNA-NUM-002).
    fn remainder(&self, other: &Self) -> Result<Self, EvaluationError> {
        if other.coefficient.is_zero() {
            return Err(error("ORNA-EVAL-DIVIDE-BY-ZERO"));
        }
        let (exponent, left, right) = self.aligned_with(other)?;
        Self::new(left % right, exponent)
    }
    /// Both coefficients scaled to the smaller exponent, with that exponent.
    fn aligned_with(&self, other: &Self) -> Result<(BigInt, BigInt, BigInt), EvaluationError> {
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
        Ok((
            exponent,
            &self.coefficient * BigInt::from(10u8).pow(left as u32),
            &other.coefficient * BigInt::from(10u8).pow(right as u32),
        ))
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
    fn list(values: Vec<Value>) -> Self {
        Self::List(List::from(values))
    }
    fn contains_callable(&self) -> bool {
        match self {
            Self::Function { .. } | Self::Closure(_) => true,
            Self::List(values) => values.iter().any(Self::contains_callable),
            Self::Tuple(values) => values.iter().any(Self::contains_callable),
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
            Self::List(values) => values.iter().any(Self::contains_float),
            Self::Tuple(values) => values.iter().any(Self::contains_float),
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

    fn nested_context_blob(&self) -> Option<&ContextValue> {
        match self {
            Self::ContextualBlob(value) => Some(value),
            Self::List(values) => values.iter().find_map(Self::nested_context_blob),
            Self::Tuple(values) => values.iter().find_map(Self::nested_context_blob),
            Self::Stream {
                values, provider, ..
            } => values
                .iter()
                .find_map(Self::nested_context_blob)
                .or_else(|| {
                    provider
                        .as_ref()
                        .and_then(|provider| provider.recover.as_deref())
                        .and_then(Self::nested_context_blob)
                }),
            Self::Record(values) => values.values().find_map(Self::nested_context_blob),
            Self::NominalRecord { fields, .. } => fields
                .iter()
                .find_map(|(_, value)| value.nested_context_blob()),
            Self::Enum { payload, .. } | Self::Option(payload) => payload
                .as_deref()
                .and_then(Self::nested_context_blob),
            Self::Range { lower, upper, .. } => lower
                .as_deref()
                .and_then(Self::nested_context_blob)
                .or_else(|| upper.as_deref().and_then(Self::nested_context_blob)),
            _ => None,
        }
    }

    fn contains_annotated_blob(&self) -> bool {
        match self {
            Self::AnnotatedBlob(_) => true,
            Self::List(values) => values.iter().any(Self::contains_annotated_blob),
            Self::Tuple(values) => values.iter().any(Self::contains_annotated_blob),
            Self::Stream {
                values, provider, ..
            } => values.iter().any(Self::contains_annotated_blob)
                || provider
                    .as_ref()
                    .and_then(|provider| provider.recover.as_deref())
                    .is_some_and(Self::contains_annotated_blob),
            Self::Record(values) => values.values().any(Self::contains_annotated_blob),
            Self::NominalRecord { fields, .. } => fields
                .iter()
                .any(|(_, value)| value.contains_annotated_blob()),
            Self::Enum { payload, .. } | Self::Option(payload) => payload
                .as_deref()
                .is_some_and(Self::contains_annotated_blob),
            Self::Range { lower, upper, .. } => lower
                .as_deref()
                .is_some_and(Self::contains_annotated_blob)
                || upper
                    .as_deref()
                    .is_some_and(Self::contains_annotated_blob),
            _ => false,
        }
    }

    fn contextual_profile(
        &self,
        fallback_format: ValueFormat,
        fallback_context: Option<&Format3Context>,
    ) -> (ValueFormat, Option<Format3Context>) {
        if let Some(value) = self.nested_context_blob() {
            return (value.format(), value.context().cloned());
        }
        if self.contains_annotated_blob() {
            return (ValueFormat::Ovb2, None);
        }
        (fallback_format, fallback_context.cloned())
    }
    fn discard_result(self) -> Result<(), EvaluationError> {
        match self {
            Self::Error(failure) => Err(failure),
            Self::Range { lower, upper, .. } => {
                if let Some(lower) = lower {
                    (*lower).discard_result()?;
                }
                if let Some(upper) = upper {
                    (*upper).discard_result()?;
                }
                Ok(())
            }
            Self::List(values) => {
                for value in values {
                    value.discard_result()?;
                }
                Ok(())
            }
            Self::Tuple(values) => {
                for value in values {
                    value.discard_result()?;
                }
                Ok(())
            }
            Self::Stream {
                values,
                position,
                provider,
                ..
            } => {
                if provider.is_some() {
                    return Ok(());
                }
                for value in values.into_iter().skip(position) {
                    value.discard_result()?;
                }
                Ok(())
            }
            Self::Record(values) => {
                for value in values.into_values() {
                    value.discard_result()?;
                }
                Ok(())
            }
            Self::NominalRecord { fields, .. } => {
                for (_, value) in fields {
                    value.discard_result()?;
                }
                Ok(())
            }
            Self::Enum { payload, .. } | Self::Option(payload) => {
                if let Some(value) = payload {
                    (*value).discard_result()?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    fn canonical(self) -> Result<CanonicalValue, EvaluationError> {
        if let Self::Error(failure) = self {
            return Err(failure);
        }
        CanonicalValue::new(self.raw()?).map_err(|_| error("ORNA-EVAL-VALUE"))
    }
    /// Encoded canonical bytes of this value, equal to
    /// `self.clone().canonical()?.encode()`, but borrowing `self` so the digest
    /// fold does not clone every row.
    fn canonical_bytes(&self) -> Result<Vec<u8>, EvaluationError> {
        if let Self::Error(failure) = self {
            return Err(failure.clone());
        }
        CanonicalValue::new(self.raw()?)
            .map_err(|_| error("ORNA-EVAL-VALUE"))?
            .encode()
            .map_err(|_| error("ORNA-EVAL-VALUE"))
    }
    /// REPL result boundary; see [`Value::canonical`]. A format-3 Blob is
    /// presented as its payload-free summary, so the result stays canonical
    /// without copying the reference's bytes or descriptor across the boundary.
    fn canonical_repl_result(self) -> Result<CanonicalValue, EvaluationError> {
        CanonicalValue::new(summarise_rov3_blobs(self.raw()?)?)
            .map_err(|_| error("ORNA-EVAL-VALUE"))
    }
    /// Canonical raw form of this value. It borrows the value, so the digest
    /// fold can encode each relation row without first deep-copying it; only
    /// the leaves and containers that the owned raw form needs are allocated.
    fn raw(&self) -> Result<Raw, EvaluationError> {
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
            Self::Error(failure) => return Err(failure.clone()),
            Self::Null => Raw::Null,
            Self::Unit => Raw::Tag(60014, Box::new(Raw::Array(vec![]))),
            Self::Bool(value) => Raw::Bool(*value),
            Self::Int(value) => Raw::Int(value.clone()),
            Self::Decimal(value) => Raw::Tag(
                60000,
                Box::new(Raw::Array(vec![
                    Raw::Int(value.coefficient.clone()),
                    Raw::Int(value.exponent10.clone()),
                ])),
            ),
            Self::Money { amount, currency } => Raw::Tag(
                60007,
                Box::new(Raw::Array(vec![
                    Raw::Tag(
                        60000,
                        Box::new(Raw::Array(vec![
                            Raw::Int(amount.coefficient.clone()),
                            Raw::Int(amount.exponent10.clone()),
                        ])),
                    ),
                    object_id_raw(*currency),
                ])),
            ),
            Self::Float(bits) => Raw::Float(*bits),
            Self::String(value) => Raw::Text(value.clone()),
            Self::Blob(value) => Raw::Bytes(value.clone()),
            Self::AnnotatedBlob(value) => value.clone(),
            Self::ContextualBlob(value) => value.raw().clone(),
            Self::Date(value) => Raw::Tag(60001, Box::new(Raw::Text(value.clone()))),
            Self::Uuid(value) => object_id_raw(*value),
            Self::Digest(value) => Raw::Tag(
                60025,
                Box::new(Raw::Array(vec![
                    Raw::Text("sha256".to_owned()),
                    Raw::Bytes(value.to_vec()),
                ])),
            ),
            Self::Reference(value) => value.raw().clone(),
            Self::TableReference(value) => value.raw().clone(),
            Self::Instant {
                unix_seconds,
                nanosecond,
            } => Raw::Tag(
                60002,
                Box::new(Raw::Array(vec![
                    Raw::Int((*unix_seconds).into()),
                    Raw::Int((*nanosecond).into()),
                ])),
            ),
            Self::Duration {
                seconds,
                nanosecond,
            } => Raw::Tag(
                60005,
                Box::new(Raw::Array(vec![
                    Raw::Int(seconds.clone()),
                    Raw::Int((*nanosecond).into()),
                ])),
            ),
            Self::Range {
                lower,
                upper,
                upper_inclusive,
            } => Raw::Tag(
                60019,
                Box::new(Raw::Array(vec![
                    raw_option_value(lower.as_deref())?,
                    raw_option_value(upper.as_deref())?,
                    Raw::Bool(*upper_inclusive),
                ])),
            ),
            Self::List(values) => Raw::Array(
                values
                    .iter()
                    .map(Value::raw)
                    .collect::<Result<_, _>>()?,
            ),
            Self::Tuple(values) if values.is_empty() => {
                Raw::Tag(60014, Box::new(Raw::Array(Vec::new())))
            }
            Self::Tuple(values) => Raw::Tag(
                60015,
                Box::new(Raw::Array(
                    values.iter().map(Value::raw).collect::<Result<_, _>>()?,
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
                        .iter()
                        .skip(*position)
                        .map(Value::raw)
                        .collect::<Result<_, _>>()?,
                )
            }
            Self::Record(values) => {
                // OVB orders map keys by complete deterministic encodings.
                // Keep the comparator tied to that rule, including CBOR head
                // length transitions and multi-byte Unicode field names.
                let mut fields = values
                    .iter()
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
                        .map(|(_, key, value)| {
                            value.raw().map(|value| (Raw::Text(key.clone()), value))
                        })
                        .collect::<Result<_, _>>()?,
                )
            }
            Self::NominalRecord { type_id, fields } => {
                // The runtime value preserves declaration order. OVB requires
                // canonical field-key order only at the serialization edge.
                let mut encoded = fields
                    .iter()
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
                        type_id.clone(),
                        Raw::Array(
                            encoded
                                .into_iter()
                                .map(|(_, key, value)| {
                                    value
                                        .raw()
                                        .map(|value| Raw::Array(vec![key.clone(), value]))
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
                    type_id.clone(),
                    variant_id.clone(),
                    payload
                        .as_deref()
                        .map(Value::raw)
                        .transpose()?
                        .unwrap_or(Raw::Null),
                ])),
            ),
            Self::Option(value) => Raw::Tag(
                60013,
                Box::new(match value.as_deref() {
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
            Raw::Tag(60025, boxed) => {
                let Raw::Array(items) = boxed.as_ref() else {
                    return Err(error("ORNA-EVAL-VALUE"));
                };
                let [Raw::Text(algorithm), Raw::Bytes(bytes)] = items.as_slice() else {
                    return Err(error("ORNA-EVAL-VALUE"));
                };
                if algorithm.as_str() != "sha256" {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                let bytes = bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| error("ORNA-EVAL-VALUE"))?;
                Ok(Self::Digest(bytes))
            }
            Raw::Tag(60010, _) => Ok(Self::Reference(value.clone())),
            Raw::Tag(60021, _) => Ok(Self::TableReference(value.clone())),
            Raw::Tag(60002, boxed) => Self::instant_from_raw(boxed, context),
            Raw::Tag(60005, boxed) => Self::duration_from_raw(boxed, context),
            Raw::Array(values) => {
                context.items(values.len())?;
                values
                    .iter()
                    .map(|value| Self::from_raw(value, context, depth + 1))
                    .collect::<Result<Vec<_>, _>>()
                    .map(Self::list)
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
        match raw {
            Raw::Tag(OVB2_BLOB_TAG, _) => {
                context.depth(depth)?;
                // The OVB-2 boundary validates the complete ContextValue
                // before evaluator conversion. Retain the validated wire
                // value without decoding and re-encoding the Blob payload.
                Ok(Self::AnnotatedBlob(raw.clone()))
            }
            Raw::Array(values) => {
                context.depth(depth)?;
                context.items(values.len())?;
                values
                    .iter()
                    .map(|value| Self::from_raw(value, context, depth + 1))
                    .collect::<Result<Vec<_>, _>>()
                    .map(Self::list)
            }
            Raw::Map(values) => {
                context.depth(depth)?;
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
            Raw::Tag(60015, boxed) => {
                context.depth(depth)?;
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
            Raw::Tag(60008, boxed) => {
                context.depth(depth)?;
                Self::enum_from_raw(boxed, context, depth)
            }
            Raw::Tag(60009, boxed) => {
                context.depth(depth)?;
                Self::nominal_record_from_raw(boxed, context, depth)
            }
            Raw::Tag(60013, boxed) => {
                context.depth(depth)?;
                Self::option_from_raw(boxed, context, depth)
            }
            Raw::Tag(60019, boxed) => {
                context.depth(depth)?;
                Self::range_from_raw(boxed, context, depth)
            }
            Raw::Tag(37 | 60000 | 60001 | 60002 | 60005 | 60007 | 60010 | 60014 | 60021, _) => {
                let value =
                    CanonicalValue::new(raw.clone()).map_err(|_| error("ORNA-EVAL-VALUE"))?;
                Self::from_canonical(&value, context, depth)
            }
            Raw::Tag(_, _) => {
                context.depth(depth)?;
                Err(error("ORNA-EVAL-UNSUPPORTED"))
            }
            _ => {
                let value =
                    CanonicalValue::new(raw.clone()).map_err(|_| error("ORNA-EVAL-VALUE"))?;
                Self::from_canonical(&value, context, depth)
            }
        }
    }
    /// Retains the original ContextValue profile and owner pin on Blob values.
    fn from_context_value(
        value: &ContextValue,
        context: &mut Context,
        depth: usize,
    ) -> Result<Self, EvaluationError> {
        fn decode(
            raw: &Raw,
            profile: &ContextValue,
            context: &mut Context,
            depth: usize,
        ) -> Result<Value, EvaluationError> {
            match raw {
                Raw::Tag(OVB2_BLOB_TAG, _) => {
                    context.depth(depth)?;
                    Ok(Value::AnnotatedBlob(raw.clone()))
                }
                Raw::Tag(ROV3_BLOB_TAG | SOV3_BLOB_TAG, _) => {
                    context.depth(depth)?;
                    let blob = match (profile.format(), profile.context()) {
                        (ValueFormat::Rov3 | ValueFormat::Sov3, Some(value_context)) => {
                            ContextValue::new_in_context(
                                value_context,
                                profile.format(),
                                raw.clone(),
                            )
                        }
                        (ValueFormat::Rov3 | ValueFormat::Sov3, None) => {
                            return Err(error("ORNA-EVAL-VALUE"));
                        }
                        (format, _) => ContextValue::new(format, raw.clone()),
                    }
                    .map_err(|_| error("ORNA-EVAL-VALUE"))?;
                    Ok(Value::ContextualBlob(blob))
                }
                Raw::Array(values) => {
                    context.depth(depth)?;
                    context.items(values.len())?;
                    values
                        .iter()
                        .map(|value| decode(value, profile, context, depth + 1))
                        .collect::<Result<Vec<_>, _>>()
                        .map(Value::list)
                }
                Raw::Tag(60015, boxed) => {
                    context.depth(depth)?;
                    let Raw::Array(values) = boxed.as_ref() else {
                        return Err(error("ORNA-EVAL-VALUE"));
                    };
                    if values.is_empty() {
                        return Err(error("ORNA-EVAL-VALUE"));
                    }
                    context.items(values.len())?;
                    values
                        .iter()
                        .map(|value| decode(value, profile, context, depth + 1))
                        .collect::<Result<Vec<_>, _>>()
                        .map(Value::Tuple)
                }
                Raw::Map(values) => {
                    context.depth(depth)?;
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
                            .insert(key.clone(), decode(value, profile, context, depth + 1)?)
                            .is_some()
                        {
                            return Err(error("ORNA-EVAL-VALUE"));
                        }
                    }
                    Ok(Value::Record(record))
                }
                Raw::Tag(60013, boxed) => {
                    context.depth(depth)?;
                    let Raw::Array(parts) = boxed.as_ref() else {
                        return Err(error("ORNA-EVAL-VALUE"));
                    };
                    match parts.as_slice() {
                        [Raw::Int(tag)] if tag.is_zero() => Ok(Value::Option(None)),
                        [Raw::Int(tag), value] if *tag == BigInt::from(1) => {
                            Ok(Value::Option(Some(Box::new(decode(
                                value,
                                profile,
                                context,
                                depth + 1,
                            )?))))
                        }
                        _ => Err(error("ORNA-EVAL-VALUE")),
                    }
                }
                _ => Value::from_raw(raw, context, depth),
            }
        }
        match (value.format(), value.raw()) {
            (ValueFormat::Ovb2, Raw::Tag(OVB2_BLOB_TAG, _)) => {
                context.depth(depth)?;
                Ok(Value::AnnotatedBlob(value.raw().clone()))
            }
            (ValueFormat::Rov3, Raw::Tag(ROV3_BLOB_TAG, _))
            | (ValueFormat::Sov3, Raw::Tag(SOV3_BLOB_TAG, _)) => {
                if value.context().is_none() {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                context.depth(depth)?;
                Ok(Value::ContextualBlob(value.clone()))
            }
            _ => decode(value.raw(), value, context, depth),
        }
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

    fn from_repl_environment(
        environment: &Environment,
        contextual_environment: &BTreeMap<String, ContextValue>,
        nominal_definitions: &NominalDefinitions,
        context: &mut Context,
    ) -> Result<Self, EvaluationError> {
        let mut scope =
            Self::from_environment_with_nominals(environment, nominal_definitions, context)?;
        let contextual_scope = Self::from_context_environment(contextual_environment, context)?;
        scope.0.extend(contextual_scope.0);
        Ok(scope)
    }

    fn from_ovb2_environment(
        environment: &BTreeMap<String, ContextValue>,
        context: &mut Context,
    ) -> Result<Self, EvaluationError> {
        context.items(environment.len())?;
        let mut values = BTreeMap::new();
        for (name, value) in environment {
            if name.len() > context.limits.max_string_bytes {
                return Err(error("ORNA-EVAL-LIMIT"));
            }
            if value.format() == ValueFormat::Ovb1 {
                return Err(error("sys.codec.unsupported_value_profile"));
            }
            if value.format() != ValueFormat::Ovb2 {
                return Err(error("ORNA-EVAL-VALUE"));
            }
            let value = Value::from_context_value(value, context, 0)?;
            values.insert(name.clone(), value);
        }
        Ok(Self(
            values,
            BTreeSet::new(),
            BTreeSet::new(),
            NominalDefinitions::new(),
            BTreeSet::new(),
        ))
    }

    fn from_context_environment(
        environment: &BTreeMap<String, ContextValue>,
        context: &mut Context,
    ) -> Result<Self, EvaluationError> {
        context.items(environment.len())?;
        let mut values = BTreeMap::new();
        for (name, value) in environment {
            if name.len() > context.limits.max_string_bytes {
                return Err(error("ORNA-EVAL-LIMIT"));
            }
            values.insert(name.clone(), Value::from_context_value(value, context, 0)?);
        }
        Ok(Self(
            values,
            BTreeSet::new(),
            BTreeSet::new(),
            NominalDefinitions::new(),
            BTreeSet::new(),
        ))
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

/// One materialization decision, keyed by `sys.Materialization`'s natural key
/// (`object` + `snapshot`). Decisions with the same key upsert one row.
struct MaterializationRecord {
    /// Deterministic identity of the relation stages that produced the value.
    object: [u8; 32],
    /// Deterministic identity of the produced output (its canonical digest).
    snapshot: [u8; 32],
    digest: [u8; 32],
    rows: usize,
    bytes: Option<usize>,
    valid: bool,
    /// Pinned `sys.MaterializationRef`, captured with the decision. `None` when
    /// no OVB-2 value context pins the activation.
    reference: Option<CanonicalValue>,
    /// Activation instant (ORNA-TXN-005) shared by every decision in one
    /// activation. A decision is recorded only when a clock resolved.
    created: Value,
}

/// Materialization decisions produced by one callback, drained by its parent
/// in callback index order so the parent log stays deterministic.
type MaterializationSink = Arc<std::sync::Mutex<Vec<MaterializationRecord>>>;

fn take_materializations(sink: &MaterializationSink) -> Vec<MaterializationRecord> {
    std::mem::take(
        &mut *sink
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )
}

/// Counts activation-local call reuse decisions. The counters are observational
/// only: reuse never changes a logical result (ORNA-PLAN-001).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CallReuseStats {
    pub hits: u64,
    pub misses: u64,
    pub invalidations: u64,
    pub stores: u64,
}

/// Bounds the per-activation memo so long activations cannot grow it without limit.
const CALL_REUSE_CAPACITY: usize = 256;

/// Activation-local memo for admitted named calls (ORNA-CALL-001..003). `epoch`
/// advances at every external boundary, so an entry is reused only while no
/// effect, write, observation or fork has happened since it was stored.
struct CallReuse {
    enabled: bool,
    epoch: u64,
    entries: VecDeque<CallReuseEntry>,
    stats: CallReuseStats,
}

impl Default for CallReuse {
    fn default() -> Self {
        Self {
            enabled: true,
            epoch: 0,
            entries: VecDeque::new(),
            stats: CallReuseStats::default(),
        }
    }
}

impl CallReuse {
    /// Fork children run concurrently with their parent, so their memo is never used.
    fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }
}

struct CallReuseEntry {
    key: CallReuseKey,
    epoch: u64,
    value: Value,
}

#[derive(PartialEq)]
struct CallReuseKey {
    callable: Value,
    captured: Scope,
    supplied: BTreeMap<ArgumentKey, Value>,
    repl_bindings: bool,
    namespace: Option<String>,
}

/// Host activation facts for `sys.rt.info()`. Without the
/// `system-host-bindings` feature no host can attach, so the slot is `None`.
#[cfg(feature = "system-host-bindings")]
type ActivationSysHost<'a> = Option<&'a SysHostAttachment>;
#[cfg(not(feature = "system-host-bindings"))]
type ActivationSysHost<'a> = Option<&'a std::convert::Infallible>;

struct Context<'functions, 'effects> {
    limits: Limits,
    steps: u64,
    /// Active source activations: function, closure and callback bodies.
    /// `Limits::max_depth` bounds this count (ORNA-CLOSURE-004).
    calls: usize,
    functions: &'functions Functions,
    aliases: Option<&'functions BTreeMap<String, String>>,
    session_functions: Option<&'functions BTreeSet<String>>,
    repl_bindings: bool,
    restrict_function_names: bool,
    reject_unhandled_field_calls: bool,
    effects: Option<&'effects mut dyn EffectHandler>,
    namespace: Option<String>,
    transfer: Option<Transfer>,
    materializations: Vec<MaterializationRecord>,
    materialization_clock_unavailable: bool,
    cancellation: Option<&'functions CancellationToken>,
    reuse: CallReuse,
    sys_host: ActivationSysHost<'functions>,
}

impl Context<'_, '_> {
    /// Advances the activation epoch at an external boundary. Entries stored
    /// before this point are stale and are never returned.
    fn invalidate_call_reuse(&mut self) {
        self.reuse.epoch = self.reuse.epoch.saturating_add(1);
    }

    /// Concurrent fork children may write while the parent continues, so the
    /// parent stops reusing results for the rest of the activation.
    fn disable_call_reuse_for_fork(&mut self) {
        self.invalidate_call_reuse();
        self.reuse.enabled = false;
        self.reuse.entries.clear();
    }

    /// Invokes an admitted named function through the activation memo. A result
    /// is stored only when its inputs and output are reusable and no external
    /// boundary was crossed while its body ran.
    fn invoke_pure_reusing(
        &mut self,
        callable: &Value,
        parameters: &[Parameter],
        body: &Expr,
        captured: Scope,
        supplied: BTreeMap<ArgumentKey, Value>,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        if !self.reuse.enabled
            || !reusable_value(callable)
            || !captured.0.values().all(reusable_value)
            || !supplied.values().all(reusable_value)
        {
            return invoke_pure(self, parameters, body, captured, supplied, depth);
        }
        let key = CallReuseKey {
            callable: callable.clone(),
            captured,
            supplied,
            repl_bindings: self.repl_bindings,
            namespace: self.namespace.clone(),
        };
        let epoch = self.reuse.epoch;
        if let Some(position) = self.reuse.entries.iter().position(|entry| entry.key == key) {
            if self.reuse.entries[position].epoch == epoch {
                self.reuse.stats.hits += 1;
                self.step()?;
                return Ok(self.reuse.entries[position].value.clone());
            }
            self.reuse.entries.remove(position);
            self.reuse.stats.invalidations += 1;
        }
        self.reuse.stats.misses += 1;
        let result = invoke_pure(
            self,
            parameters,
            body,
            key.captured.clone(),
            key.supplied.clone(),
            depth,
        );
        if let Ok(value) = &result {
            if self.reuse.enabled && self.reuse.epoch == epoch && reusable_value(value) {
                if self.reuse.entries.len() == CALL_REUSE_CAPACITY {
                    self.reuse.entries.pop_front();
                }
                self.reuse.stats.stores += 1;
                self.reuse.entries.push_back(CallReuseEntry {
                    key,
                    epoch,
                    value: value.clone(),
                });
            }
        }
        result
    }
}

/// Closures and lazy stream/relation plans carry live evaluation state, so
/// they are never stored or matched as reusable values.
fn reusable_value(value: &Value) -> bool {
    match value {
        Value::Closure(_) | Value::Stream { .. } | Value::Relation(_) => false,
        Value::List(values) => values.iter().all(reusable_value),
        Value::Tuple(values) => values.iter().all(reusable_value),
        Value::Record(fields) => fields.values().all(reusable_value),
        Value::NominalRecord { fields, .. } => fields.iter().all(|(_, field)| reusable_value(field)),
        Value::Enum { payload, .. } => payload.as_deref().map_or(true, reusable_value),
        Value::Option(payload) => payload.as_deref().map_or(true, reusable_value),
        Value::Range { lower, upper, .. } => {
            lower.as_deref().map_or(true, reusable_value)
                && upper.as_deref().map_or(true, reusable_value)
        }
        _ => true,
    }
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

/// Runs one task callback on a worker thread. The child starts with the
/// owner's active calls, so recursion through task combinators stays within
/// `Limits::max_depth`.
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
    sys_host: ActivationSysHost<'_>,
    active_calls: usize,
    mut effects: Option<Box<dyn EffectHandler + Send>>,
    materializations: &MaterializationSink,
) -> Result<Value, EvaluationError> {
    let mut context = Context {
        limits,
        steps: 0,
        calls: active_calls,
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
        materializations: Vec::new(),
        materialization_clock_unavailable: false,
        reuse: CallReuse::disabled(),
        sys_host,
        cancellation: Some(&cancellation),
    };
    let result = context.invoke_callable(&callback, Vec::new(), 0);
    materializations
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .extend(std::mem::take(&mut context.materializations));
    result
}

/// Run finite callback lists with a bounded number of owned worker threads.
/// A first ordinary failure stops admission for `parallel`; a first success
/// stops admission for `race`. In-flight children observe the shared token,
/// and `thread::scope` joins every worker before the owner returns.
fn run_task_callbacks(
    callbacks: &[Value],
    mut child_effects: Vec<Option<Box<dyn EffectHandler + Send>>>,
    active_calls: usize,
    parent: Option<CancellationToken>,
    functions: &Functions,
    aliases: Option<&BTreeMap<String, String>>,
    session_functions: Option<&BTreeSet<String>>,
    limits: Limits,
    repl_bindings: bool,
    restrict_function_names: bool,
    reject_unhandled_field_calls: bool,
    sys_host: ActivationSysHost<'_>,
    cancel_after_failure: bool,
    cancel_after_success: bool,
) -> (
    Vec<Option<Result<Value, EvaluationError>>>,
    Option<Value>,
    Vec<Vec<MaterializationRecord>>,
) {
    const MAX_TASK_WORKERS: usize = 32;
    let available = std::thread::available_parallelism().map_or(1, usize::from);
    let worker_count = callbacks.len().min(available).min(MAX_TASK_WORKERS).max(1);
    let next = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let shared_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let child_effects = Arc::new(std::sync::Mutex::new(std::mem::take(&mut child_effects)));

    let sinks: Arc<Vec<MaterializationSink>> = Arc::new(
        (0..callbacks.len())
            .map(|_| Arc::new(std::sync::Mutex::new(Vec::new())))
            .collect(),
    );
    let (results, winner) = std::thread::scope(|scope| {
        let (sender, receiver) = std::sync::mpsc::channel();
        for _ in 0..worker_count {
            let sender = sender.clone();
            let next = Arc::clone(&next);
            let shared_cancel = Arc::clone(&shared_cancel);
            let child_effects = Arc::clone(&child_effects);
            let sinks = Arc::clone(&sinks);
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
                            sys_host,
                            active_calls,
                            effects,
                            &sinks[index],
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
    });
    let records = sinks.iter().map(take_materializations).collect();
    (results, winner, records)
}

impl Context<'_, '_> {
    fn has_ovb2_effects(&self) -> bool {
        self.effects
            .as_deref()
            .is_some_and(EffectHandler::is_ovb2_binding)
    }

    fn ovb2_effects(&mut self) -> Option<&mut dyn Ovb2EffectHandler> {
        self.effects.as_deref_mut()?.ovb2_binding()
    }

    fn effect_value(&mut self, value: &CanonicalValue) -> Result<Value, EvaluationError> {
        // Effect results cross a canonical boundary. Apply the depth limit to
        // the returned value's own structure from its root, independently of
        // the source call stack. Otherwise a handler can commit an effect and
        // result decoding can then fail only because that caller was deep.
        Value::from_canonical(value, self, 0)
    }

    fn ovb2_effect_value(&mut self, value: &ContextValue) -> Result<Value, EvaluationError> {
        let (format, accepts_context_values) = {
            let handler = self
                .ovb2_effects()
                .ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?;
            (handler.value_format(), handler.accepts_context_values())
        };
        if !accepts_context_values
            && (value.format() != format || format != ValueFormat::Ovb2)
        {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        if accepts_context_values {
            Value::from_context_value(value, self, 0)
        } else {
            Value::from_raw(value.raw(), self, 0)
        }
    }

    fn ovb2_context_value(value: Value) -> Result<ContextValue, EvaluationError> {
        ContextValue::new(ValueFormat::Ovb2, value.raw()?).map_err(|_| error("ORNA-EVAL-VALUE"))
    }

    fn effect_context_value(&mut self, value: Value) -> Result<ContextValue, EvaluationError> {
        let (format, value_context, accepts_context_values) = {
            let handler = self
                .ovb2_effects()
                .ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?;
            (
                handler.value_format(),
                handler.value_context().cloned(),
                handler.accepts_context_values(),
            )
        };
        if !accepts_context_values {
            return Self::ovb2_context_value(value);
        }
        Self::contextual_value(value, format, value_context.as_ref())
    }

    fn contextual_value(
        value: Value,
        format: ValueFormat,
        value_context: Option<&Format3Context>,
    ) -> Result<ContextValue, EvaluationError> {
        let (format, value_context) = value.contextual_profile(format, value_context);
        let raw = value.raw()?;
        match format {
            ValueFormat::Rov3 | ValueFormat::Sov3 => ContextValue::new_in_context(
                value_context.as_ref().ok_or_else(|| error("ORNA-EVAL-VALUE"))?,
                format,
                raw,
            ),
            ValueFormat::Ovb1 | ValueFormat::Ovb2 => ContextValue::new(format, raw),
        }
        .map_err(|_| error("ORNA-EVAL-VALUE"))
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
    /// One more activation must fit within `Limits::max_depth` active calls.
    fn check_call_room(&self) -> Result<(), EvaluationError> {
        self.depth(self.calls.saturating_add(1))
    }
    fn enter_call(&mut self) -> Result<(), EvaluationError> {
        self.check_call_room()?;
        self.calls += 1;
        Ok(())
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
    ) -> Result<Value, EvaluationError> {
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
                    let value = Value::from_canonical(&canonical, self, 1)?;
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
    ) -> Result<Value, EvaluationError> {
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
            return Value::from_canonical(&key, self, 1);
        }

        self.invalidate_call_reuse();
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
        match Value::from_canonical(&target, self, 0)? {
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
    fn field_value(
        &mut self,
        base: Value,
        name: &str,
        scope: &mut Scope,
    ) -> Result<Value, EvaluationError> {
        match base {
            Value::Record(fields) => fields
                .get(name)
                .cloned()
                .ok_or_else(|| error("ORNA-EVAL-FIELD")),
            Value::NominalRecord { type_id, fields } => {
                self.nominal_field(&type_id, &fields, name, scope)
            }
            Value::Reference(reference) => self.reference_field(&reference, name, scope),
            Value::Error(failure) => self.error_field(&failure, name),
            Value::Int(value) => self.numeric_postfix(value, name),
            Value::Decimal(value) => self.numeric_decimal_postfix(value, name),
            _ if self.transfer.is_some() => Ok(Value::Null),
            _ => Err(error("ORNA-EVAL-TYPE")),
        }
    }
    fn evaluate(
        &mut self,
        expression: &Expr,
        scope: &mut Scope,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        // Expression nesting inside one body is bounded by the parser and
        // `Limits::max_depth` bounds active calls, but debug frames for the
        // evaluator's large match arms can exceed a test thread's default
        // stack well before those bounds. Grow onto a fresh segment near the limit.
        stacker::maybe_grow(EVALUATOR_STACK_RED_ZONE, EVALUATOR_STACK_SEGMENT, || {
            self.evaluate_node(expression, scope, depth)
        })
    }
    fn evaluate_node(
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
        let result = match expression {
            Expr::Name { text, .. } => scope
                .0
                .get(text)
                .cloned()
                .or_else(|| {
                    (self.bare_user_names_visible() && self.functions.contains_key(text)).then(
                        || Value::Function {
                            name: text.clone(),
                            nominal_definitions: scope.3.clone(),
                        },
                    )
                })
                .or_else(|| {
                    self.alias_target(text)
                        .filter(|name| self.functions.contains_key(name))
                        .map(str::to_owned)
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
            Expr::List { elements, .. } => self.sequence(elements, scope, depth).map(Value::list),
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
                if let Some((enum_name, variant)) = system_enum_constant(expression, scope) {
                    return Ok(Value::Enum {
                        type_id: Raw::Text(enum_name),
                        variant_id: Raw::Text(variant),
                        payload: None,
                    });
                }
                match payload_free_enum_variant(base, name, scope, self.namespace.as_deref()) {
                    Some(variant) => variant,
                    None => {
                        let base = self.evaluate(base, scope, depth + 1)?;
                        self.field_value(base, name, scope)
                    }
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
                    if !bind(binding, value, &mut iteration, self, 1)? {
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
                                if let Some(value) = iteration.0.remove(name) {
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
                        if let Some(value) = iteration.0.remove(name) {
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
        let Some(definition) = scope
            .3
            .get(qualified.as_deref().unwrap_or_default())
            .or_else(|| scope.3.get(&spelling))
            .cloned()
        else {
            return self.enum_variant_construction(path, supplied, scope, depth);
        };
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
    fn enum_variant_construction(
        &mut self,
        path: &[orna_syntax_v1::NameSegment],
        supplied: &[orna_syntax_v1::RecordField],
        scope: &Scope,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        // ORNA-ENUM-003: `Enum.variant { field: value }` constructs a payload
        // variant whose record fields are exactly the declared payload fields.
        let [owner @ .., variant] = path else {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        };
        if owner.is_empty() {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        let owner = owner
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<Vec<_>>()
            .join(".");
        let Some((type_id, declaration)) =
            enum_variant_identity(scope, self.namespace.as_deref(), &owner, &variant.text)
        else {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        };
        let mut supplied_names = BTreeSet::new();
        for field in supplied {
            if !declaration.payload_fields.contains(&field.name)
                || !supplied_names.insert(field.name.clone())
            {
                return Err(error("ORNA-EVAL-ARGUMENT"));
            }
        }
        if supplied_names.len() != declaration.payload_fields.len() {
            return Err(error("ORNA-EVAL-ARGUMENT"));
        }
        let mut payload = BTreeMap::new();
        for field in supplied {
            let value = self.evaluate(&field.value, &mut scope.clone(), depth + 1)?;
            if self.transfer.is_some() {
                return Ok(Value::Null);
            }
            payload.insert(field.name.clone(), value);
        }
        let payload = (!declaration.payload_fields.is_empty())
            .then(|| Box::new(Value::Record(payload)));
        Ok(Value::Enum {
            type_id: type_id.clone(),
            variant_id: declaration.variant_id.clone(),
            payload,
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
                // ORNA-LIT-002: conversion is correctly rounded to nearest, so an
                // out-of-range numeral yields a signed infinity rather than an error.
                Ok(Value::Float(value.to_bits()))
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
                StringSegment::Text { text, .. } => output.push_str(
                    &orna_syntax_v1::unescape_string_body(text)
                        .map_err(|_| error("ORNA-EVAL-VALUE"))?,
                ),
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
                    if !bind(pattern, value, &mut local, self, 1)? {
                        return Err(error("ORNA-EVAL-TYPE"));
                    }
                    declared_names.extend(pattern_names(pattern));
                }
                Statement::Assert { value, .. } => {
                    match self.evaluate(value, &mut local, depth + 1)? {
                        Value::Bool(true) => {}
                        Value::Bool(false) => return Err(error("ORNA-A091-008")),
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
            if let Some(value) = local.0.remove(&name) {
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
        // Compound operators read the slot before the right-hand side runs. A
        // plain `=` only requires the slot, so it does not copy the old value.
        let current = match operator {
            AssignmentOperator::Set if scope.0.contains_key(name) => None,
            _ => Some(
                scope
                    .0
                    .get(name)
                    .cloned()
                    .ok_or_else(|| error("ORNA-EVAL-NAME"))?,
            ),
        };
        let value = self.evaluate(expression, scope, depth + 1)?;
        if self.transfer.is_some() {
            return Ok(());
        }
        let value = match (operator, current) {
            (AssignmentOperator::Set, _) => value,
            (AssignmentOperator::Add, Some(current)) => self.apply_binary("+", current, value)?,
            (AssignmentOperator::Subtract, Some(current)) => {
                self.apply_binary("-", current, value)?
            }
            (AssignmentOperator::Multiply, Some(current)) => {
                self.apply_binary("*", current, value)?
            }
            (AssignmentOperator::Divide, Some(current)) => {
                self.apply_binary("/", current, value)?
            }
            (_, None) => return Err(error("ORNA-EVAL-NAME")),
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
            ("-", Value::Float(value)) => Ok(float_value(-f64::from_bits(value))),
            _ => Err(error("ORNA-EVAL-TYPE")),
        }
    }

    /// A table name piped into a relation operation reads that table: a bare
    /// `music` or a root-qualified `main.music`. Locals, functions, aliases and
    /// namespace-qualified functions keep their precedence over a table name,
    /// mirroring [`Expr::Name`] resolution in [`Context::evaluate_node`].
    fn admitted_table_relation(&self, lhs: &Expr, scope: &Scope) -> Option<String> {
        let path = dotted_name_path(lhs)?;
        let root = path.split('.').next()?;
        if scope.0.contains_key(root) {
            return None;
        }
        if !self.restrict_function_names && self.functions.contains_key(&path) {
            return None;
        }
        if self
            .alias_target(&path)
            .is_some_and(|name| self.functions.contains_key(name))
        {
            return None;
        }
        if let Some(namespace) = self.namespace.as_deref() {
            if self.functions.contains_key(&format!("{namespace}.{path}")) {
                return None;
            }
        }
        self.effects.as_deref()?.admitted_relation_source(&path)
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
                    None => match self.admitted_table_relation(lhs, scope) {
                        Some(source) => Value::Relation(RelationPlan::new(source)),
                        None => self.evaluate(lhs, scope, depth + 1)?,
                    },
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
                _ => blob_aware_eq(&left, &right)?,
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
                Ok(values.into_vec())
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
            "%" => a.remainder(&b),
            // ORNA-COMPARE-002: ordered comparison of two Decimals yields Bool,
            // exactly and independently of scale (1.5 == 1.50).
            "<" | "<=" | ">" | ">=" => {
                return compare(op, compare_values(&Value::Decimal(a), &Value::Decimal(b))?);
            }
            _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
        .map(Value::Decimal)
    }
    fn decimal_divide_method(
        &mut self,
        numerator: DecimalValue,
        arguments: &[orna_syntax_v1::Argument],
        scope: &mut Scope,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        let mut values = Vec::with_capacity(arguments.len());
        for argument in arguments {
            let shorthand_rounding = argument.name.as_deref() == Some("rounding")
                && matches!(
                    &argument.value,
                    Expr::Name { text, .. } if text == "half_even"
                );
            let value = if shorthand_rounding {
                Value::String("half_even".into())
            } else {
                self.evaluate(&argument.value, scope, depth + 1)?
            };
            if self.transfer.is_some() {
                return Ok(Value::Null);
            }
            values.push(value);
        }
        let values = named_arguments("decimal.divide", arguments, values, 0, false)?;
        match values.as_slice() {
            [Value::Decimal(divisor), Value::Int(scale), Value::String(rounding)] => {
                if scale.is_negative() {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                if rounding != "half_even" {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                let scale = scale.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
                if scale > self.limits.max_integer_digits {
                    return Err(error("ORNA-EVAL-LIMIT"));
                }
                self.checked_decimal(numerator.divide_rounded(divisor, scale)?)
                    .map(Value::Decimal)
            }
            _ => Err(error("ORNA-EVAL-TYPE")),
        }
    }
    fn float_binary(&self, op: &str, a: u64, b: u64) -> Result<Value, EvaluationError> {
        let (a, b) = (f64::from_bits(a), f64::from_bits(b));
        match op {
            "+" => Ok(float_value(a + b)),
            "-" => Ok(float_value(a - b)),
            "*" => Ok(float_value(a * b)),
            "/" => Ok(float_value(a / b)),
            "%" => Ok(float_value(a % b)),
            "^" => Ok(float_value(libm::pow(a, b))),
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
            self.functions,
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
                ordered[0] = Value::list(rows);
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
                    *value = Value::list(self.collect_relation_values(plan, depth + 1)?);
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
            Value::List(rows) => Ok(Value::Relation(RelationPlan::from_values(rows.into_vec()))),
            // `partition` has a scalar tuple result whose two components each
            // preserve the source container kind.
            Value::Tuple(parts) if name == "partition" && parts.len() == 2 => {
                let mut relations = Vec::with_capacity(2);
                for part in parts {
                    let Value::List(rows) = part else {
                        return Err(error("ORNA-EVAL-VALUE"));
                    };
                    relations.push(Value::Relation(RelationPlan::from_values(rows.into_vec())));
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
            shared_filter_group: plan.shared_filter_group,
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
                shared_filter_group: plan.shared_filter_group,
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
                self.collection("sort_by", vec![Value::list(groups), key.clone()], depth)?
            else {
                unreachable!("sort_by returns a list");
            };
            let mut distinct_seen = vec![HashSet::new(); plan.stages.len()];
            let mut pair_previous = vec![None; plan.stages.len()];
            let mut window_states = (0..plan.stages.len())
                .map(|_| None)
                .collect::<Vec<Option<RelationWindowState>>>();
            return self.for_each_buffered_relation(
                sorted.into_vec(),
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
                    Value::list(bucket.values),
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
                self.for_each_bucket_group(plan, bucket_index, depth, |_, _| {
                    count = count
                        .checked_add(1)
                        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
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
                self.for_each_relation_value_with_page_limit(
                    plan,
                    depth,
                    1,
                    |_, value| {
                    first = Some(value);
                    Ok(false)
                })?;
                Ok(Value::Option(first.map(Box::new)))
            }
            "one" => {
                let predicate = arguments.first();
                let mut found = None;
                self.for_each_relation_value_with_page_limit(
                    plan,
                    depth,
                    1,
                    |context, value| {
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
                self.for_each_relation_value(plan, depth, |_, _| {
                    count = count
                        .checked_add(1)
                        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
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
                // Fold in chunks of at most one item budget. Each chunk starts
                // with the running result, which keeps the relation's
                // left-to-right order, so a Float sum rounds exactly like a
                // list sum and nothing beyond one chunk is retained.
                let limit = self.limits.max_collection_items;
                let mut chunk = Vec::new();
                let mut digest = OutputDigest::new();
                self.for_each_relation_value(plan, depth, |context, value| {
                    digest.push(&value);
                    if chunk.len() >= limit {
                        let values = std::mem::take(&mut chunk);
                        let running =
                            context.observe_list_relation(operation, values, arguments, depth)?;
                        chunk.push(match running {
                            Value::Option(Some(running)) => *running,
                            running => running,
                        });
                    }
                    chunk.push(value);
                    Ok(true)
                })?;
                self.record_materialization(plan, digest)?;
                self.observe_list_relation(operation, chunk, arguments, depth)
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
        let mut inputs = vec![Value::list(values)];
        inputs.extend(arguments.iter().cloned());
        self.collection(operation, inputs, depth)
    }

    fn collect_relation_values(
        &mut self,
        plan: &RelationPlan,
        depth: usize,
    ) -> Result<Vec<Value>, EvaluationError> {
        let mut values = Vec::new();
        let mut digest = OutputDigest::new();
        self.for_each_relation_value(plan, depth, |context, value| {
            digest.push(&value);
            values.push(value);
            context.items(values.len())?;
            Ok(true)
        })?;
        self.record_materialization(plan, digest)?;
        Ok(values)
    }

    fn for_each_relation_value(
        &mut self,
        plan: &RelationPlan,
        depth: usize,
        visit: impl FnMut(&mut Self, Value) -> Result<bool, EvaluationError>,
    ) -> Result<(), EvaluationError> {
        self.for_each_relation_value_with_page_limit(
            plan,
            depth,
            RELATION_SCAN_PAGE_ROWS,
            visit,
        )
    }

    fn for_each_relation_value_with_page_limit(
        &mut self,
        plan: &RelationPlan,
        depth: usize,
        page_limit: usize,
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
        // A04-05: a scan charges every streamed row one step. Only values that
        // a stage or terminal retains count against `max_collection_items`.
        self.for_each_relation_source_value(plan, depth, page_limit, &mut |context, value| {
            context.step()?;
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
        page_limit: usize,
        visit: &mut dyn FnMut(&mut Self, Value) -> Result<bool, EvaluationError>,
    ) -> Result<(), EvaluationError> {
        if let Some((left, right)) = &plan.source_union {
            let mut keep_going = true;
            self.for_each_relation_value_with_page_limit(
                left,
                depth,
                page_limit,
                |context, value| {
                keep_going = visit(context, value)?;
                Ok(keep_going)
            })?;
            if !keep_going {
                return Ok(());
            }
            self.for_each_relation_value_with_page_limit(
                right,
                depth,
                page_limit,
                |context, value| {
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
            let (rows, next) = self.relation_page(
                &plan.source,
                plan.source_identity,
                after.as_deref(),
                page_limit,
            )?;
            let page_len = rows.len();
            for value in rows {
                self.step()?;
                if !visit(self, value)? {
                    return Ok(());
                }
            }
            let Some(next) = next else {
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
                    value = Value::list(window);
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
            shared_filter_group: plan.shared_filter_group,
            source_values: plan.source_values.clone(),
            stages: plan.stages[..sort_index].to_vec(),
        };
        let RelationStage::SortBy(key) = &plan.stages[sort_index] else {
            unreachable!("sort stage index")
        };
        let mut values = Vec::new();
        let mut digest = OutputDigest::new();
        self.for_each_relation_value(&prefix, depth, |context, value| {
            digest.push(&value);
            values.push(value);
            context.items(values.len())?;
            Ok(true)
        })?;
        self.record_materialization(&prefix, digest)?;
        let sorted = self.collection("sort_by", vec![Value::list(values), key.clone()], depth)?;
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
            sorted.into_vec(),
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
        let sorted = self.collection("sort_by", vec![Value::list(upstream), key.clone()], depth)?;
        let Value::List(sorted) = sorted else {
            unreachable!("sort_by returns a list")
        };
        self.for_each_buffered_relation(
            sorted.into_vec(),
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
        page_limit: usize,
    ) -> Result<(Vec<Value>, Option<Vec<u8>>), EvaluationError> {
        if source == SYS_MATERIALIZATION_SOURCE {
            return self.materialization_page(after, page_limit);
        }
        enum SourcePage {
            Canonical(RelationPage),
            Contextual(ContextValueRelationPage),
        }

        self.invalidate_call_reuse();
        let remaining = self.limits.max_steps.saturating_sub(self.steps);
        let mut budget = StepBudget::new(remaining);
        let result = self
            .effects
            .as_deref_mut()
            .ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))
            .and_then(|effects| {
                let contextual = if let Some(handler) = effects.ovb2_binding() {
                    handler.scan_context_relation_page_scoped(
                        source,
                        scope,
                        after,
                        page_limit,
                        &mut budget,
                    )?
                } else {
                    None
                };
                if let Some(page) = contextual {
                    Ok(Some(SourcePage::Contextual(page)))
                } else {
                    effects
                        .scan_relation_page_scoped(
                            source,
                            scope,
                            after,
                            page_limit,
                            &mut budget,
                        )
                        .map(|page| page.map(SourcePage::Canonical))
                }
            });
        let debited = remaining.saturating_sub(budget.remaining());
        self.steps = self
            .steps
            .checked_add(debited)
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        let page = result?.ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?;
        let page_size = match &page {
            SourcePage::Canonical(page) => page.rows.len(),
            SourcePage::Contextual(page) => page.rows.len(),
        };
        if page_size > self.limits.max_collection_items {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        let (rows, next) = match page {
            SourcePage::Canonical(page) => page
                .rows
                .iter()
                .map(|row| Value::from_canonical(row, self, 0))
                .collect::<Result<Vec<_>, _>>()
                .map(|rows| (rows, page.next)),
            SourcePage::Contextual(page) => {
                let (format, accepts_context_values) = {
                    let handler = self
                        .ovb2_effects()
                        .ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?;
                    (handler.value_format(), handler.accepts_context_values())
                };
                let rows = page
                    .rows
                    .iter()
                    .map(|row| {
                        if !accepts_context_values
                            && (row.format() != format || format != ValueFormat::Ovb2)
                        {
                            return Err(error("ORNA-EVAL-VALUE"));
                        }
                        Value::from_context_value(row, self, 0)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok((rows, page.next))
            }
        }?;
        if let (Some(after), Some(next)) = (after, next.as_deref())
            && next <= after
        {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        Ok((rows, next))
    }

    /// Records a materialization decision for `sys.Materialization`. The key
    /// is `object` (stage identity) plus `snapshot` (output digest). A repeated
    /// key is already in the log and is not recorded again. Only the canonical
    /// digest, count, and byte size are retained; rows are not copied.
    /// Only buffered or folded scans reach this function: collect, sort,
    /// sum/min/max and list paths. Pipelines that never buffer (count, first,
    /// exists) record no decision; ORNA-CALL-004 permits that, and the
    /// omission is deliberate. A decision with no resolvable clock is also not
    /// recorded (see `materialization_clock`).
    fn record_materialization(
        &mut self,
        plan: &RelationPlan,
        digest: OutputDigest,
    ) -> Result<(), EvaluationError> {
        if plan.source == SYS_MATERIALIZATION_SOURCE {
            return Ok(());
        }
        let object = relation_stage_digest(plan);
        let rows = digest.rows;
        let (digest, bytes, valid) = digest.finish();
        let snapshot = sha256_parts("orna.materialization.snapshot.v1", &[&digest]);
        if self.has_materialization(&object, &snapshot) {
            return Ok(());
        }
        let Some(created) = self.materialization_clock()? else {
            return Ok(());
        };
        let reference = self.materialization_reference(&object, &snapshot)?;
        self.admit_materialization(MaterializationRecord {
            object,
            snapshot,
            digest,
            rows,
            bytes,
            valid,
            reference,
            created,
        })
    }

    fn has_materialization(&self, object: &[u8; 32], snapshot: &[u8; 32]) -> bool {
        self.materializations
            .iter()
            .any(|record| record.object == *object && record.snapshot == *snapshot)
    }

    /// Pins the decision's `sys.MaterializationRef` to this activation's OVB-2
    /// value context. Without that context there is no database or snapshot
    /// pin, so no reference is fabricated and the row omits `reference`.
    fn materialization_reference(
        &mut self,
        object: &[u8; 32],
        snapshot: &[u8; 32],
    ) -> Result<Option<CanonicalValue>, EvaluationError> {
        let Some(handler) = self.ovb2_effects() else {
            return Ok(None);
        };
        let Some(context) = handler.value_context() else {
            return Ok(None);
        };
        let reference = orna_foundation_v1::materialization_reference(
            context.database_uuid(),
            context.pin().clone(),
            *object,
            *snapshot,
        )
        .map_err(|_| error("ORNA-EVAL-VALUE"))?;
        reference
            .into_row_ref()
            .to_canonical()
            .map(Some)
            .map_err(|_| error("ORNA-EVAL-VALUE"))
    }

    /// Appends a decision unless its key is already logged. The log holds one
    /// row per distinct key and is bounded by `max_collection_items`.
    fn admit_materialization(
        &mut self,
        record: MaterializationRecord,
    ) -> Result<(), EvaluationError> {
        if self.has_materialization(&record.object, &record.snapshot) {
            return Ok(());
        }
        if self.materializations.len() >= self.limits.max_collection_items {
            return Err(error("ORNA-EVAL-LIMIT"));
        }
        self.materializations.push(record);
        Ok(())
    }

    /// Admits materialization decisions made by task callbacks. Batches are
    /// given in callback index order, so the parent log is deterministic.
    fn absorb_materializations(
        &mut self,
        batches: Vec<Vec<MaterializationRecord>>,
    ) -> Result<(), EvaluationError> {
        for record in batches.into_iter().flatten() {
            self.admit_materialization(record)?;
        }
        Ok(())
    }

    /// Serves one page of the activation's materialization log. The cursor is
    /// the 8-byte big-endian index of the next entry.
    fn materialization_page(
        &mut self,
        after: Option<&[u8]>,
        page_limit: usize,
    ) -> Result<(Vec<Value>, Option<Vec<u8>>), EvaluationError> {
        let start = match after {
            None => 0,
            Some(cursor) => {
                let bytes: [u8; 8] = cursor.try_into().map_err(|_| error("ORNA-EVAL-VALUE"))?;
                usize::try_from(u64::from_be_bytes(bytes)).map_err(|_| error("ORNA-EVAL-VALUE"))?
            }
        };
        let total = self.materializations.len();
        if start > total {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        let end = start.saturating_add(page_limit).min(total);
        if end == start {
            return Ok((Vec::new(), None));
        }
        if end - start > self.limits.max_collection_items {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        let mut rows = Vec::with_capacity(end - start);
        for index in start..end {
            self.step()?;
            rows.push(materialization_row(&self.materializations[index]));
        }
        let next = (end < total).then(|| (end as u64).to_be_bytes().to_vec());
        Ok((rows, next))
    }

    /// Reads the activation clock through the effect boundary, debiting the
    /// activation's step budget. `None` means no handler resolved `now`; the
    /// evaluator never supplies a clock itself.
    fn activation_now(&mut self, callee: &Expr) -> Result<Option<Value>, EvaluationError> {
        let remaining = self.limits.max_steps.saturating_sub(self.steps);
        let mut budget = StepBudget::new(remaining);
        let result = self.resolve_now(callee, &mut budget);
        let debited = remaining - budget.remaining();
        self.steps = self
            .steps
            .checked_add(debited)
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        result
    }

    /// Dispatches a `now` call to the OVB-2 handler when present, otherwise to
    /// the plain effect handler, and converts the answer to a value. The caller
    /// owns the step budget.
    fn resolve_now(
        &mut self,
        callee: &Expr,
        budget: &mut StepBudget,
    ) -> Result<Option<Value>, EvaluationError> {
        if self.effects.is_none() {
            return Ok(None);
        }
        if self.has_ovb2_effects() {
            let result = self
                .ovb2_effects()
                .expect("checked OVB-2 effect handler")
                .handle_with_budget(callee, &[], budget)?;
            return match result {
                Some(value) => self.ovb2_effect_value(&value).map(Some),
                None => Ok(None),
            };
        }
        let result = self
            .effects
            .as_deref_mut()
            .expect("checked effect handler")
            .handle_with_budget(callee, &[], budget)?;
        match result {
            Some(value) => self.effect_value(&value).map(Some),
            None => Ok(None),
        }
    }

    /// The activation instant for a new decision. Every decision in one
    /// activation shares the instant its first decision captured (ORNA-TXN-005),
    /// so later decisions reuse it. The read runs on a budget of its own and is
    /// not debited from the query, so recording cannot move a pure result to a
    /// step limit (ORNA-PLAN-001). The clock is a host-visible `now` effect call,
    /// made at most once per activation: when it is missing, failing or not an
    /// Instant the decision is not recorded rather than failing the query or
    /// recording a fabricated `created`, and that absence is cached so later
    /// decisions in the same activation do not call the host again. Cancellation
    /// still propagates.
    fn materialization_clock(&mut self) -> Result<Option<Value>, EvaluationError> {
        if let Some(first) = self.materializations.first() {
            return Ok(Some(first.created.clone()));
        }
        if self.materialization_clock_unavailable {
            return Ok(None);
        }
        let mut budget = StepBudget::new(self.limits.max_steps);
        let clock = match self.resolve_now(&materialization_clock_callee(), &mut budget) {
            Ok(Some(instant @ Value::Instant { .. })) => Some(instant),
            Err(failure) if failure.code() == "ORNA-EVAL-CANCELLED" => return Err(failure),
            _ => None,
        };
        if clock.is_none() {
            self.materialization_clock_unavailable = true;
        }
        Ok(clock)
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
        } else if plan.source == SYS_MATERIALIZATION_SOURCE {
            Ok(())
        } else {
            self.effects
                .as_deref_mut()
                .ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?
                .validate_relation_source(&plan.source)
        }
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
            return json_node_to_value(node, self, 0);
        };
        let Some(definition) = scope.3.get(witness).cloned() else {
            return json_node_to_value(node, self, 0);
        };
        if !definition.variants.is_empty() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
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
                let _ = json_node_to_value(value, self, 1)?;
            } else {
                return Err(error("ORNA-EVAL-VALUE"));
            }
        }

        let mut construction_scope = scope.clone();
        let mut fields = Vec::with_capacity(definition.fields.len());
        for field in &definition.fields {
            let value = if field.public {
                if let Some(node) = supplied.remove(&field.name) {
                    json_node_to_value(node, self, 1)?
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

    #[cfg(feature = "system-host-bindings")]
    fn call_sys_explain(
        &mut self,
        arguments: &[orna_syntax_v1::Argument],
        input: Option<Value>,
        scope: &mut Scope,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        // Mirrors semantic admission: piped input is the explained value and
        // takes no argument; otherwise exactly one argument is required.
        let explained = match input {
            Some(input) => {
                if !arguments.is_empty() {
                    return Err(error("ORNA-EVAL-ARGUMENT"));
                }
                input
            }
            None => {
                if arguments.len() != 1 {
                    return Err(error("ORNA-EVAL-ARGUMENT"));
                }
                self.evaluate(&arguments[0].value, scope, depth + 1)?
            }
        };
        let catalogue = self
            .effects
            .as_deref()
            .and_then(|handler| handler.explain_catalogue());
        // Semantic admission selects the overload by static argument type
        // (source/34-system-reference.md `sys.explain`): a query is a relation,
        // a `sys.FunctionRef` is a row reference and a `sys.Diagnostic` is its
        // row record.
        match explained {
            Value::Relation(plan) => explain_bindings::explain_query_plan(&plan, catalogue),
            Value::Reference(function) => {
                explain_bindings::explain_function_plan(&function, catalogue)
            }
            Value::Record(diagnostic) => explain_bindings::explain_diagnostic(diagnostic),
            _ => Err(error("ORNA-EVAL-TYPE")),
        }
    }

    /// Answers `sys.rt.info()` from the activation facts the host attached.
    ///
    /// The call takes no argument and no piped input. Without an attached host
    /// context the activation cannot describe itself, so it reports
    /// `sys.version.incompatible` rather than a fabricated record (SYS-022..030).
    #[cfg(feature = "system-host-bindings")]
    fn call_sys_rt_info(
        &self,
        arguments: &[orna_syntax_v1::Argument],
        input: Option<Value>,
    ) -> Result<Value, EvaluationError> {
        if input.is_some() || !arguments.is_empty() {
            return Err(error("ORNA-EVAL-ARGUMENT"));
        }
        let Some(attachment) = self
            .effects
            .as_deref()
            .and_then(|handler| handler.sys_host_context())
            .or(self.sys_host)
        else {
            return Err(error("sys.version.incompatible"));
        };
        attachment.runtime_info()
    }

    /// Dispatches one portable annotated-Blob call declared by the generated
    /// typed registry.
    ///
    /// Parameter names, order, declared types, `null` defaults and the result
    /// type all come from the generated contract, and the seam itself is
    /// admitted only for operations bound to the generated Blob annotation
    /// role, so a source-level `sys.blob.*` call cannot reach native code the
    /// registry has not declared.
#[cfg(feature = "system-host-bindings")]
    fn call_blob_operation(
        &mut self,
        operation: &str,
        arguments: &[orna_syntax_v1::Argument],
        input: Option<Value>,
        scope: &mut Scope,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        if !crate::sys_bindings::is_blob_host_operation(operation) {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        let table = orna_sys_v1::system_dispatch_table();
        let contract = table
            .operation(operation)
            .ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?;
        let parameters = &contract.signature.parameters;

        let mut supplied = (0..parameters.len()).map(|_| None).collect::<Vec<_>>();
        let has_input = input.is_some();
        if let Some(input) = input {
            supplied[0] = Some(input);
        }
        // Leading positional arguments fill the next free slot in declaration
        // order; named arguments may follow them (spec examples/valid/blob-range.orna:
        // `sys.blob.read(value, offset: 1, length: 2)`), never precede them.
        let mut next_positional = usize::from(has_input);
        let mut named_seen = false;
        for argument in arguments {
            let position = match argument.name.as_deref() {
                None => {
                    if named_seen || next_positional >= parameters.len() {
                        return Err(error("ORNA-EVAL-ARGUMENT"));
                    }
                    let position = next_positional;
                    next_positional += 1;
                    position
                }
                Some(name) => {
                    named_seen = true;
                    let Some(position) = parameters
                        .iter()
                        .position(|parameter| parameter.name == name)
                    else {
                        return Err(error("ORNA-EVAL-ARGUMENT"));
                    };
                    if supplied[position].is_some() {
                        return Err(error("ORNA-EVAL-ARGUMENT"));
                    }
                    position
                }
            };
            let value = self.evaluate(&argument.value, scope, depth + 1)?;
            supplied[position] = Some(value);
            if self.transfer.is_some() {
                return Ok(Value::Null);
            }
        }

        let mut supplied_values = Vec::with_capacity(parameters.len());
        let mut bounds = Vec::new();
        for (position, parameter) in parameters.iter().enumerate() {
            let value = match supplied[position].take() {
                Some(value) => value,
                None if parameter.default.as_deref() == Some("null") => Value::Null,
                None => return Err(error("ORNA-EVAL-ARGUMENT")),
            };
            if crate::sys_bindings::is_blob_from_bytes_operation(operation) {
                let Value::List(items) = value else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                if items.len() as u64 > crate::sys_bindings::MAX_BLOB_CALL_BYTES {
                    return Err(error("sys.blob.limit"));
                }
                let integers = items
                    .iter()
                    .map(|item| match item {
                        Value::Int(integer) => Ok(integer.clone()),
                        _ => Err(error("ORNA-EVAL-TYPE")),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let annotated = crate::sys_bindings::dispatch_blob_from_bytes(&integers)?;
                return Value::from_context_value(&annotated, self, 0);
            }
            if matches!(&parameter.ty, orna_sys_v1::AbiType::Named(name) if name == "Int") {
                let Value::Int(bound) = value else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                bounds.push(bound);
            } else {
                supplied_values.push(Self::blob_annotation_argument(value, &parameter.ty)?);
            }
        }
        if crate::sys_bindings::is_blob_read_operation(operation) {
            let source = supplied_values
                .first()
                .ok_or_else(|| error("ORNA-EVAL-ARGUMENT"))?;
            return match crate::sys_bindings::dispatch_blob_read(operation, source, &bounds)? {
                crate::sys_bindings::BlobReadValue::Int(length) => {
                    Ok(Value::Int(BigInt::from(length)))
                }
                crate::sys_bindings::BlobReadValue::Blob(value) => {
                    Value::from_context_value(&value, self, 0)
                }
                crate::sys_bindings::BlobReadValue::Bytes(bytes) => Ok(Value::List(List::from(
                    bytes
                        .into_iter()
                        .map(|byte| Value::Int(BigInt::from(byte)))
                        .collect::<Vec<_>>(),
                ))),
                crate::sys_bindings::BlobReadValue::Unit => Ok(Value::Unit),
                crate::sys_bindings::BlobReadValue::Digest(value) => Ok(Value::Digest(value)),
            };
        }
        if crate::sys_bindings::is_blob_reflection_operation(operation) {
            return crate::sys_bindings::dispatch_blob_reflection(
                operation,
                &supplied_values,
                self.cancellation,
            );
        }
        let annotated = crate::sys_bindings::dispatch_blob_annotation(operation, &supplied_values)?;
        Value::from_context_value(&annotated, self, 0)
    }

    /// Converts one source-level argument into the value accepted by the
    /// generated Blob ABI. A contextual Blob keeps its original profile.
    ///
    /// The declared type comes from the generated contract, so the conversion
    /// cannot invent a shape the declaration does not allow. Unannotated bytes
    /// remain unsupported rather than receiving a lossy fallback annotation.
#[cfg(feature = "system-host-bindings")]
    fn blob_annotation_argument(
        value: Value,
        declared: &orna_sys_v1::AbiType,
    ) -> Result<ContextValue, EvaluationError> {
        let optional = matches!(declared, orna_sys_v1::AbiType::Optional(_));
        let expected = match declared {
            orna_sys_v1::AbiType::Named(name) => name.as_str(),
            orna_sys_v1::AbiType::Optional(inner) => match inner.as_ref() {
                orna_sys_v1::AbiType::Named(name) => name.as_str(),
                _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
            },
            _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
        };
        let raw = match (expected, value) {
            ("Blob", Value::AnnotatedBlob(raw)) => raw,
            ("Blob", Value::ContextualBlob(value)) => return Ok(value),
            ("Blob", Value::Blob(_)) => return Err(error("ORNA-EVAL-UNSUPPORTED")),
            ("Str", Value::String(text)) => Raw::Text(text),
            ("Str", Value::Null | Value::Option(None)) if optional => Raw::Null,
            ("Str", Value::Option(Some(value))) if optional => match *value {
                Value::String(text) => Raw::Text(text),
                _ => return Err(error("ORNA-EVAL-TYPE")),
            },
            _ => return Err(error("ORNA-EVAL-TYPE")),
        };
        ContextValue::new(ValueFormat::Ovb2, raw).map_err(|_| error("ORNA-EVAL-TYPE"))
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
        let mut pre_evaluated_callee = None;
        if resolved_name.is_none()
            && input.is_none()
            && let Expr::Field { base, name, .. } = callee
            && name == "divide"
        {
            let receiver = self.evaluate(base, scope, depth + 1)?;
            if self.transfer.is_some() {
                return Ok(Value::Null);
            }
            match receiver {
                Value::Decimal(numerator) => {
                    return self.decimal_divide_method(numerator, arguments, scope, depth + 1);
                }
                receiver => {
                    pre_evaluated_callee =
                        Some(self.field_value(receiver, name, scope)?);
                }
            }
        }
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
            self.invalidate_call_reuse();
            if let Some(value) = self.activation_now(callee)? {
                return Ok(value);
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
            self.invalidate_call_reuse();
            let remaining = self.limits.max_steps.saturating_sub(self.steps);
            let mut budget = StepBudget::new(remaining);
            if self.has_ovb2_effects() {
                let result = self
                    .ovb2_effects()
                    .expect("checked OVB-2 effect handler")
                    .handle_with_budget(callee, &[], &mut budget);
                let debited = remaining - budget.remaining();
                self.steps = self
                    .steps
                    .checked_add(debited)
                    .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
                if let Some(value) = result? {
                    return self.ovb2_effect_value(&value);
                }
            } else {
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
        let source_function_name = function_name(callee);
        // Every `sys.explain` overload is evaluated natively against the host
        // catalogue, never by a provider invoke. An unshadowed call therefore
        // stays on this path.
        #[cfg(feature = "system-host-bindings")]
        if source_function_name.as_deref() == Some("sys.explain")
            && !scope.0.contains_key("sys")
        {
            return self.call_sys_explain(arguments, input, scope, depth);
        }
        // `sys.rt.info()` reports the activation the host attached. Like
        // `sys.explain`, it is lowered here, never by a provider invoke.
        #[cfg(feature = "system-host-bindings")]
        if source_function_name.as_deref() == Some("sys.rt.info")
            && !scope.0.contains_key("sys")
        {
            return self.call_sys_rt_info(arguments, input);
        }

        // A portable Blob call is admitted only when the generated typed
        // registry declares the operation. The resolved name wins when a
        // declaration was reachable; an unresolved, unshadowed source-level
        // `sys.blob.*` call is accepted so the generated declarations stay
        // callable, and a resolved non-Blob function never falls through.
        #[cfg(feature = "system-host-bindings")]
        let declared = match resolved_function.as_deref() {
            Some(name) if crate::sys_bindings::is_blob_host_operation(name) => {
                Some(name.to_owned())
            }
            Some(_) => None,
            None => source_function_name
                .as_deref()
                .filter(|name| {
                    !scope.0.contains_key("sys")
                        && crate::sys_bindings::is_blob_host_operation(name)
                })
                .map(str::to_owned),
        }
        .filter(|name| {
            orna_sys_v1::system_dispatch_table()
                .operation(name)
                .is_some()
        });
        #[cfg(feature = "system-host-bindings")]
        if let Some(operation) = declared {
            return self.call_blob_operation(&operation, arguments, input, scope, depth);
        }
        #[cfg(feature = "system-host-bindings")]
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
            if self.has_ovb2_effects() {
                let values = ordered_arguments
                    .iter()
                    .map(|argument| {
                        let value = self.evaluate(&argument.value, scope, depth + 1)?;
                        self.effect_context_value(value)
                    })
                    .collect::<Result<Vec<_>, EvaluationError>>()?;
                self.invalidate_call_reuse();
                let remaining = self.limits.max_steps.saturating_sub(self.steps);
                let mut budget = StepBudget::new(remaining);
                let cancellation = self.cancellation;
                let result = self
                    .ovb2_effects()
                    .expect("checked OVB-2 effect handler")
                    .handle_registered_with_cancellation_and_budget(
                        operation_name,
                        callee,
                        &values,
                        &mut budget,
                        cancellation,
                    );
                let debited = remaining - budget.remaining();
                self.steps = self
                    .steps
                    .checked_add(debited)
                    .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
                if let Some(value) = result? {
                    return self.ovb2_effect_value(&value);
                }
            } else {
                // Keep the legacy conversion and dispatch order intact for
                // every existing EffectHandler caller.
                let values = ordered_arguments
                    .iter()
                    .map(|argument| {
                        self.evaluate(&argument.value, scope, depth + 1)?
                            .canonical()
                    })
                    .collect::<Result<Vec<_>, EvaluationError>>()?;
                self.invalidate_call_reuse();
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
            self.functions,
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
                && captured_collection_binding_is_native(binding.operation, self.functions)
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
                if self.has_ovb2_effects() {
                    let values = values
                        .into_iter()
                        .map(|value| self.effect_context_value(value))
                        .collect::<Result<Vec<_>, _>>()?;
                    self.invalidate_call_reuse();
                    let remaining = self.limits.max_steps.saturating_sub(self.steps);
                    let mut budget = StepBudget::new(remaining);
                    let result = self
                        .ovb2_effects()
                        .expect("checked OVB-2 effect handler")
                        .handle_with_budget(callee, &values, &mut budget);
                    let debited = remaining - budget.remaining();
                    self.steps = self
                        .steps
                        .checked_add(debited)
                        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
                    if let Some(value) = result? {
                        return self.ovb2_effect_value(&value);
                    }
                } else {
                    let values = values
                        .into_iter()
                        .map(Value::canonical)
                        .collect::<Result<Vec<_>, _>>()?;
                    self.invalidate_call_reuse();
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
            } else if let Some(callable) = pre_evaluated_callee {
                callable
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
            self.check_call_room()?;
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
            let result = self.invoke_pure_reusing(
                &callable,
                parameters,
                body,
                captured,
                supplied,
                depth + 1,
            );
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
            if self.restrict_function_names && nullable {
                // ORNA-TYPE-001: `T?` is Option<T>; absence is Option none, not a bare null.
                Ok(match value {
                    Value::Null => Value::Option(None),
                    Value::Option(_) => value,
                    value => Value::Option(Some(Box::new(value))),
                })
            } else {
                Ok(value)
            }
        } else if time.is_some() {
            self.time(name, values)
        } else if stream.is_some() {
            self.stream(name, values, depth)
        } else if concurrent.is_some() {
            self.concurrent(name, values)
        } else if native_ui.is_some() {
            self.ui_node(values)
        } else if native_hash.is_some() {
            self.hash(name, values)
        } else if native_random.is_some() {
            self.random(name, values, callee)
        } else if base64.is_some() {
            self.base64(name, values)
        } else if json.is_some() {
            self.json_codec(name, values, &scope.3)
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
    /// Bare (unqualified) names resolve to top-level functions only outside named
    /// standard modules. A standard body resolves its own module first and never
    /// sees a user function through a bare name (ORNA-LIB-001 isolation).
    fn bare_user_names_visible(&self) -> bool {
        let in_standard = self
            .namespace
            .as_deref()
            .is_some_and(|namespace| namespace == "std" || namespace.starts_with("std."));
        !self.restrict_function_names && !in_standard
    }
    /// Resolves an import alias key. The REPL's session aliases apply first. A
    /// bare key is the admitted module's own import and is visible only outside
    /// a named module, so a standard body never resolves a bare name through a
    /// user alias. A module-internal key (`namespace::name`) resolves through the
    /// shared standard layer.
    fn alias_target(&self, key: &str) -> Option<&str> {
        if let Some(target) = self.aliases.and_then(|aliases| aliases.get(key)) {
            return Some(target.as_str());
        }
        if !key.contains("::") && self.namespace.is_some() {
            return None;
        }
        self.functions.alias(key)
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
            if let Some(alias) = self.alias_target(&module_alias)
                && self.functions.contains_key(alias)
            {
                return Some(alias.to_owned());
            }
        }
        // Bare session aliases come from top-level REPL imports (ORNA-IMPORT-003,
        // ORNA-IMPORT-005) and bind only in that lexical scope. A body inside a
        // named module resolves its own imports through `namespace::name` keys
        // above, so a REPL `use std.prelude as _;` must not capture bare names
        // such as `count` inside `std.list` (which would recurse through the
        // prelude back into `std.list.length`).
        if self.namespace.is_none()
            && let Some(alias) = self.alias_target(&name)
            && self.functions.contains_key(alias)
        {
            return Some(alias.to_owned());
        }
        #[cfg(feature = "system-host-bindings")]
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
        if self.bare_user_names_visible() && self.functions.contains_key(&name) {
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
            Value::List(values) => values
                .get(index)
                .cloned()
                .ok_or_else(|| error("ORNA-EVAL-INDEX")),
            Value::Tuple(values) => values
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
            if !bind(&arm.pattern, value.clone(), &mut local, self, 1)? {
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
            let result = self.evaluate(&arm.body, &mut local, depth + 1)?;
            // ORNA-CFLOW-005: assignments in the arm replace the enclosing
            // function-local slots, as in a block; names the arm pattern binds
            // stay local to the arm.
            let bound_names = pattern_names(&arm.pattern);
            let outer_names = scope.0.keys().cloned().collect::<Vec<_>>();
            for name in outer_names {
                if bound_names.contains(&name) {
                    continue;
                }
                if let Some(value) = local.0.remove(&name) {
                    scope.0.insert(name, value);
                }
            }
            return Ok(result);
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
                Ok(Value::list(fields))
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
        self.invalidate_call_reuse();
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
                Value::list(values)
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
                self.finite_stream(source_label.clone(), items.to_vec())
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
                    batches.push(Value::list(batch.to_vec()));
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
    fn concurrent(&mut self, name: &str, values: Vec<Value>) -> Result<Value, EvaluationError> {
        match (name, values.as_slice()) {
            ("parallel", [Value::List(callbacks)]) => {
                self.items(callbacks.len())?;
                if callbacks.is_empty() {
                    return Ok(Value::list(Vec::new()));
                }
                self.check_call_room()?;
                let parent = self.cancellation.cloned();
                let mut child_effects = Vec::with_capacity(callbacks.len());
                self.disable_call_reuse_for_fork();
                if let Some(effects) = self.effects.as_deref_mut() {
                    for index in 0..callbacks.len() {
                        child_effects.push(effects.fork_task_child(index));
                    }
                } else {
                    child_effects.resize_with(callbacks.len(), || None);
                }
                let (results, _, records) = run_task_callbacks(
                    callbacks,
                    child_effects,
                    self.calls,
                    parent.clone(),
                    self.functions,
                    self.aliases,
                    self.session_functions,
                    self.limits,
                    self.repl_bindings,
                    self.restrict_function_names,
                    self.reject_unhandled_field_calls,
                    self.sys_host,
                    true,
                    false,
                );
                self.absorb_materializations(records)?;
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
                Ok(Value::list(output))
            }
            ("race", [Value::List(callbacks)]) => {
                self.items(callbacks.len())?;
                if callbacks.is_empty() {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                self.check_call_room()?;
                let parent = self.cancellation.cloned();
                let mut child_effects = Vec::with_capacity(callbacks.len());
                self.disable_call_reuse_for_fork();
                if let Some(effects) = self.effects.as_deref_mut() {
                    for index in 0..callbacks.len() {
                        child_effects.push(effects.fork_task_child(index));
                    }
                } else {
                    child_effects.resize_with(callbacks.len(), || None);
                }
                let (results, winner, records) = run_task_callbacks(
                    callbacks,
                    child_effects,
                    self.calls,
                    parent.clone(),
                    self.functions,
                    self.aliases,
                    self.session_functions,
                    self.limits,
                    self.repl_bindings,
                    self.restrict_function_names,
                    self.reject_unhandled_field_calls,
                    self.sys_host,
                    false,
                    true,
                );
                self.absorb_materializations(records)?;
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
                self.check_call_room()?;
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
                let sys_host = self.sys_host;
                let active_calls = self.calls;
                self.disable_call_reuse_for_fork();
                let child_effects = self
                    .effects
                    .as_deref_mut()
                    .and_then(|effects| effects.fork_task_child(0));
                let materializations: MaterializationSink =
                    Arc::new(std::sync::Mutex::new(Vec::new()));
                let result = std::thread::scope(|scope| {
                    let (sender, receiver) = std::sync::mpsc::channel();
                    let shared_for_task = Arc::clone(&shared_cancel);
                    let callback = callback.clone();
                    let task_parent = parent.clone();
                    let sink_for_task = Arc::clone(&materializations);
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
                                sys_host,
                                active_calls,
                                child_effects,
                                &sink_for_task,
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
                });
                self.absorb_materializations(vec![take_materializations(&materializations)])?;
                result
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
                self.string(orna_value_v1::base64_encode(bytes)).map(Value::String)
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
                let mut shuffled = values.to_vec();
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
                Ok(Value::list(shuffled))
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
        self.invalidate_call_reuse();
        let remaining = self.limits.max_steps.saturating_sub(self.steps);
        let mut budget = StepBudget::new(remaining);
        let cancellation = self.cancellation;
        let value_raw = if self.has_ovb2_effects() {
            let arguments = [self.effect_context_value(Value::Int(BigInt::from(count)))?];
            let result = self
                .ovb2_effects()
                .expect("checked OVB-2 effect handler")
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
            if value.format() != ValueFormat::Ovb2 {
                return Err(error("ORNA-EVAL-VALUE"));
            }
            value.raw().clone()
        } else {
            let count = CanonicalValue::new(Raw::Int(BigInt::from(count)))
                .map_err(|_| error("ORNA-EVAL-VALUE"))?;
            let arguments = [count];
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
            result?
                .ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?
                .raw()
                .clone()
        };
        let Raw::Bytes(bytes) = value_raw else {
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
    fn json_codec(
        &mut self,
        name: &str,
        values: Vec<Value>,
        nominals: &NominalDefinitions,
    ) -> Result<Value, EvaluationError> {
        match (name, values.as_slice()) {
            ("__encode", [value]) => {
                let mut output = String::new();
                write_json_value(value, &mut output, self, nominals, 0)?;
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
                let mut parsed = parse_expression(input);
                let mut bindings = BTreeMap::new();
                if !parsed.is_ok()
                    || !admit_orna_data_expression(&mut parsed.value, &mut bindings)
                {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                let mut scope = Scope(
                    bindings,
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
                    .map(Value::list)
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
            encoded_values.push(value.raw()?);
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
                        groups.push(Value::list(bucket.values));
                        self.items(groups.len())?;
                    }
                }
                if let Some(bucket) = state.finish() {
                    self.items(bucket.values.len())?;
                    groups.push(Value::list(bucket.values));
                    self.items(groups.len())?;
                }
                Ok(Value::list(groups))
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
                    chunks.push(Value::list(chunk.to_vec()));
                    self.items(chunks.len())?;
                }
                Ok(Value::list(chunks))
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
                Ok(Value::list(flattened))
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
                Ok(Value::list(pairs))
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

        Ok(Value::list(fold.into_values()))
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
        Ok(Value::list(mapped))
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
        Ok(Value::list(flattened))
    }
    fn sort_by(
        &mut self,
        values: &[Value],
        key: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut keys = Vec::with_capacity(values.len());
        for value in values {
            // Evaluate every callback in source order before considering any
            // reordering. This preserves effect/error observability even when
            // the eventual sort can be optimized by its caller.
            let sort_key = self.invoke_predicate(key, value.clone(), depth + 1)?;
            lawful_sort_key(&sort_key)?;
            keys.push(sort_key);
            self.items(keys.len())?;
        }
        let order = self.stable_order(&keys)?;
        Ok(Value::List(
            order.into_iter().map(|index| values[index].clone()).collect(),
        ))
    }
    fn rank(
        &mut self,
        values: &[Value],
        key: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut keys = Vec::with_capacity(values.len());
        for value in values {
            // Callback evaluation is observable and must consume one bounded
            // step even when the callback body is only an identity lookup.
            self.step()?;
            let sort_key = self.invoke_predicate(key, value.clone(), depth + 1)?;
            lawful_sort_key(&sort_key)?;
            keys.push(sort_key);
            self.items(keys.len())?;
        }
        let order = self.stable_order(&keys)?;
        let mut ranked = Vec::with_capacity(order.len());
        let mut competition_rank = 1usize;
        for (position, &index) in order.iter().enumerate() {
            if position > 0 {
                self.step()?;
                if compare_sort_keys(&keys[order[position - 1]], &keys[index])?
                    != std::cmp::Ordering::Equal
                {
                    competition_rank = position
                        .checked_add(1)
                        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
                }
            }
            let rank = self.integer(BigInt::from(competition_rank))?;
            ranked.push(Value::Tuple(vec![values[index].clone(), Value::Int(rank)]));
            self.items(ranked.len())?;
        }
        Ok(Value::list(ranked))
    }
    /// Stable merge sort of `0..keys.len()` by key. Each comparison costs one
    /// step and incomparable keys fail explicitly; equal keys keep source
    /// order. O(n log n) comparisons replace the former insertion sorts.
    fn stable_order(&mut self, keys: &[Value]) -> Result<Vec<usize>, EvaluationError> {
        let mut order = (0..keys.len()).collect::<Vec<_>>();
        let mut merged = Vec::with_capacity(order.len());
        let mut width = 1;
        while width < order.len() {
            merged.clear();
            for start in (0..order.len()).step_by(width * 2) {
                let middle = (start + width).min(order.len());
                let end = (start + width * 2).min(order.len());
                let (mut left, mut right) = (start, middle);
                while left < middle && right < end {
                    self.step()?;
                    if compare_sort_keys(&keys[order[right]], &keys[order[left]])?
                        == std::cmp::Ordering::Less
                    {
                        merged.push(order[right]);
                        right += 1;
                    } else {
                        merged.push(order[left]);
                        left += 1;
                    }
                }
                merged.extend_from_slice(&order[left..middle]);
                merged.extend_from_slice(&order[right..end]);
            }
            std::mem::swap(&mut order, &mut merged);
            width *= 2;
        }
        Ok(order)
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
        Ok(Value::list(fold.into_values()))
    }
    /// Concatenates `left` then `right`. Appending at the end of the longest
    /// view of a list reuses its storage, so the charge covers only the
    /// elements actually written: the appended ones and any copied prefix.
    fn union(&mut self, left: &List, right: &[Value]) -> Result<Value, EvaluationError> {
        self.items(left.len())?;
        self.items(right.len())?;
        let length = left
            .len()
            .checked_add(right.len())
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        self.items(length)?;
        let (values, copied) = left.concat(right);
        for _ in 0..copied + right.len() {
            self.step()?;
        }
        Ok(Value::List(values))
    }
    fn count(&mut self, values: &[Value]) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        // A finite list knows its length: one bounded step, not one per item.
        self.step()?;
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
            return Ok(float_value(squares / values.len() as f64));
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
            Value::Float(bits) => Ok(float_value(f64::from_bits(bits).sqrt())),
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
            (Value::Float(start), Value::Float(end)) => Ok(float_value(
                (f64::from_bits(*end) - f64::from_bits(*start)) / decimal_to_f64(&elapsed)?,
            )),
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
        Ok(Value::list(derivatives))
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
            return Ok(float_value(total));
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
                Ok(float_value(total / values.len() as f64))
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
                Ok(float_value((f64::from_bits(*left) + f64::from_bits(*right)) / 2.0))
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
            (Value::Float(lower), Value::Float(upper)) => Ok(float_value(
                f64::from_bits(*lower)
                    + (f64::from_bits(*upper) - f64::from_bits(*lower)) * decimal_to_f64(fraction)?,
            )),
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
            return Ok(float_value(
                f64::from_bits(sorted[lower])
                    + (f64::from_bits(sorted[upper]) - f64::from_bits(sorted[lower])) * fraction,
            ));
        }
        if interpolation == "midpoint" {
            return Ok(float_value(
                (f64::from_bits(sorted[lower]) + f64::from_bits(sorted[upper])) / 2.0,
            ));
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
        let mut multiple = false;
        for value in values {
            self.step()?;
            match self.invoke_predicate(predicate, value.clone(), depth + 1)? {
                Value::Bool(true) => {
                    if matching.is_some() {
                        multiple = true;
                    } else {
                        matching = Some(value.clone());
                    }
                }
                Value::Bool(false) => {}
                _ => return Err(error("ORNA-EVAL-TYPE")),
            }
        }
        if multiple {
            return Err(error("ORNA-EVAL-RELATION-ONE-MULTIPLE"));
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
        Ok(Value::list(result))
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
        Ok(Value::list(result))
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
        Ok(Value::list(filtered))
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
            Value::list(matching),
            Value::list(remaining),
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
                    groups.push(Value::list(std::mem::take(&mut current)));
                    self.items(groups.len())?;
                }
                Value::Bool(_) => {}
                _ => return Err(error("ORNA-EVAL-TYPE")),
            }
            current.push(value.clone());
            self.items(current.len())?;
        }
        if !current.is_empty() {
            groups.push(Value::list(current));
            self.items(groups.len())?;
        }
        Ok(Value::list(groups))
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
                .map(|(key, rows)| Value::Tuple(vec![key, Value::list(rows)]))
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
            return Ok(Value::list(Vec::new()));
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
        Ok(Value::list(joined))
    }
    fn invoke_callable(
        &mut self,
        callable: &Value,
        arguments: Vec<Value>,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        let functions = self.functions;
        let (parameters, body, mut captured, session_owned, namespace) = match callable {
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
        let result = invoke_pure(self, parameters, body, captured, supplied, depth + 1);
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
        let functions = self.functions;
        let (parameters, body, captured, session_owned, namespace) = match callable {
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
        let result = invoke_pure(self, parameters, body, captured, supplied, depth + 1);
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
        Ok(Value::list(pairs))
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
            return Ok(Value::list(Vec::new()));
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
            windows.push(Value::list(window.to_vec()));
            self.items(windows.len())?;
            let Some(next) = start.checked_add(step) else {
                break;
            };
            start = next;
        }
        Ok(Value::list(windows))
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
            return Ok(Value::list(Vec::new()));
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
                let result = self.stats(statistic, vec![Value::list(window)])?;
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
        Ok(Value::list(results))
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

/// Strict RFC 4648 decoding is owned by `orna-value-v1`; this wrapper only
/// maps its rejection onto the evaluator diagnostic.
fn decode_base64(input: &str) -> Result<Vec<u8>, EvaluationError> {
    orna_value_v1::base64_decode(input).map_err(|_| error("ORNA-EVAL-VALUE"))
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
                .map(Value::list)
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
    nominals: &NominalDefinitions,
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
        Value::List(_) | Value::Tuple(_) => {
            let values: &[Value] = match value {
                Value::List(values) => values,
                Value::Tuple(values) => values,
                _ => unreachable!("the arm matched a list or tuple"),
            };
            context.items(values.len())?;
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_json_value(value, output, context, nominals, depth + 1)?;
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
                write_json_value(value, output, context, nominals, depth + 1)?;
            }
            output.push('}');
        }
        Value::Option(None) => output.push_str("null"),
        Value::Option(Some(value)) => {
            write_json_value(value, output, context, nominals, depth + 1)?;
        }
        Value::Money { amount, currency } => {
            let metadata = nominals
                .values()
                .find(|definition| object_id_key(&definition.type_id) == Some(*currency))
                .and_then(|definition| definition.currency.as_ref())
                .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
            output.push_str("{\"amount\":");
            let amount = decimal_json_token(amount)?;
            output.push_str(
                &serde_json::to_string(&amount).map_err(|_| error("ORNA-EVAL-VALUE"))?,
            );
            output.push_str(",\"currency\":");
            output.push_str(
                &serde_json::to_string(&metadata.code).map_err(|_| error("ORNA-EVAL-VALUE"))?,
            );
            output.push('}');
        }
        Value::Unit
        | Value::Blob(_)
        | Value::AnnotatedBlob(_)
        | Value::ContextualBlob(_)
        | Value::Date(_)
        | Value::Uuid(_)
        | Value::Digest(_)
        | Value::Reference(_)
        | Value::TableReference(_)
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
        Value::Blob(value) => encode_orna_string(&orna_value_v1::base64_encode(value)),
        Value::AnnotatedBlob(_) => return Err(error("ORNA-EVAL-UNSUPPORTED")),
        Value::ContextualBlob(_) => return Err(error("ORNA-EVAL-UNSUPPORTED")),
        Value::Date(value) => format!("date{}", encode_orna_string(value)),
        Value::Instant { .. } | Value::Duration { .. } | Value::Money { .. } => {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        Value::Uuid(value) => encode_orna_string(&format_uuid_bytes(value)),
        Value::Digest(value) => encode_orna_string(&format_digest_hex(value)),
        Value::TableReference(value) => encode_orna_string(&orna_value_v1::base64_encode(
            &value.encode().map_err(|_| error("ORNA-EVAL-VALUE"))?,
        )),
        Value::Reference(value) => encode_orna_string(&orna_value_v1::base64_encode(
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

/// The canonical nonfinite Float spellings (29-formats.md:125) and the scope binding carrying each.
/// The NUL prefix cannot occur in a source identifier, so the binding never collides with user names.
fn nonfinite_float_constant(expression: &Expr) -> Option<(&'static str, f64)> {
    let Expr::Field { base, name, .. } = expression else {
        return None;
    };
    if !matches!(base.as_ref(), Expr::Name { text, .. } if text == "Float") {
        return None;
    }
    match name.as_str() {
        "nan" => Some(("\0Float.nan", f64::NAN)),
        "infinity" => Some(("\0Float.infinity", f64::INFINITY)),
        _ => None,
    }
}

/// ORNA-FORMAT-004: admits only the data subset of canonical Orna text. A canonical nonfinite
/// Float spelling is rewritten to a binding holding its IEEE-754 value, so no `Float` path is
/// evaluated. Returns false for any construct that is not data.
fn admit_orna_data_expression(
    expression: &mut Expr,
    bindings: &mut BTreeMap<String, Value>,
) -> bool {
    if let Some((binding, value)) = nonfinite_float_constant(expression) {
        let span = expression.span();
        bindings.insert(binding.to_owned(), float_value(value));
        *expression = Expr::Name {
            text: binding.to_owned(),
            span,
        };
        return true;
    }
    match expression {
        Expr::Literal { .. } => true,
        Expr::Name { .. } => false,
        Expr::Group { inner, .. } => admit_orna_data_expression(inner, bindings),
        Expr::Unary { op, rhs, .. } => {
            matches!(op.as_str(), "+" | "-") && admit_orna_data_expression(rhs, bindings)
        }
        Expr::List { elements, .. } | Expr::Tuple { elements, .. } => elements
            .iter_mut()
            .all(|element| admit_orna_data_expression(element, bindings)),
        Expr::Record { fields, .. } => fields
            .iter_mut()
            .all(|field| admit_orna_data_expression(&mut field.value, bindings)),
        Expr::Field { base, .. } => matches!(
            &**base,
            Expr::Literal {
                kind: LiteralKind::Decimal | LiteralKind::Integer,
                ..
            }
        ),
        Expr::Call {
            callee, arguments, ..
        } if matches!(&**callee, Expr::Name { text, .. } if text == "Some")
            && arguments.len() == 1
            && arguments.iter().all(|argument| argument.name.is_none()) =>
        {
            arguments
                .iter_mut()
                .all(|argument| admit_orna_data_expression(&mut argument.value, bindings))
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

/// Renders a Digest as lowercase 64-hex; it is never an implicit string value.
fn format_digest_hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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
/// ORNA-ENUM-002: `Enum.variant` (or `module.Enum.variant`) evaluates to a
/// payload-free variant of an admitted enum. A local binding shadows the
/// enum name, and a payload variant has no bare value form (ORNA-ENUM-003).
fn payload_free_enum_variant(
    base: &Expr,
    variant: &str,
    scope: &Scope,
    namespace: Option<&str>,
) -> Option<Result<Value, EvaluationError>> {
    if scope.3.is_empty() || scope.0.contains_key(function_root_name(base)?) {
        return None;
    }
    let owner = function_name(base)?;
    let (type_id, declaration) = enum_variant_identity(scope, namespace, &owner, variant)?;
    Some(if declaration.payload_fields.is_empty() {
        Ok(Value::Enum {
            type_id: type_id.clone(),
            variant_id: declaration.variant_id.clone(),
            payload: None,
        })
    } else {
        Err(error("ORNA-EVAL-ARGUMENT"))
    })
}

/// Resolves `owner.variant` against the admitted enums. A declaration of the
/// evaluating module shadows a same-named enum reached by its short name.
fn enum_variant_identity<'scope>(
    scope: &'scope Scope,
    namespace: Option<&str>,
    owner: &str,
    variant: &str,
) -> Option<(&'scope Raw, &'scope NominalVariant)> {
    if let Some(definition) = namespace
        .and_then(|namespace| scope.3.get(&format!("{namespace}.{owner}")))
        .or_else(|| scope.3.get(owner))
    {
        return definition
            .variants
            .iter()
            .find(|candidate| candidate.name == variant)
            .map(|candidate| (&definition.type_id, candidate));
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
        found = Some((&definition.type_id, candidate));
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
    let Some((expected_type, expected_variant)) =
        enum_variant_identity(scope, context.namespace.as_deref(), &owner, &variant)
            .map(|(type_id, candidate)| (type_id, &candidate.variant_id))
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
            | "decimal.divide"
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
        "decimal.divide" => match values.len() {
            3 => &["value", "scale", "rounding"],
            _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
        },
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
    functions: &Functions,
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
    // A bare spelling of a root intrinsic is served natively; only qualified or
    // aliased `std.collection.<name>` spellings depend on the captured pin.
    let root_intrinsic_spelling = spelling.as_deref() == Some(operation);
    if module.kind == StandardBindingKind::Collection
        && !root_intrinsic_spelling
        && LEGACY_ROOT_WRAPPER_OPERATIONS.contains(&operation)
        && !legacy_root_wrapper_is_pinned(module.prefix, operation, functions)
    {
        return None;
    }
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
    functions: &Functions,
) -> bool {
    registered_standard_binding(
        callee,
        resolved_function,
        scope,
        allow_unresolved_qualified,
        source_export_available,
        true,
        functions,
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

/// A pinned `*_from` helper whose body is a `while` loop. Historical pins
/// define these helpers recursively; the loop rewrite makes the Orna body the
/// evaluation target, while historical pins keep their native bindings
/// (ORNA-LIB-001).
fn captured_collection_helper_is_loop(functions: &Functions, name: &str) -> bool {
    let Some(helper) = functions.get(name) else {
        return false;
    };
    matches!(
        &helper.body,
        Expr::Block { statements, .. }
            if statements.iter().any(|statement| matches!(
                statement,
                Statement::Control {
                    value: Expr::Control {
                        kind: ControlKind::While,
                        ..
                    },
                    ..
                }
            ))
    )
}

/// Root intrinsics that pinned collection and query modules used to re-declare
/// as `std.<module>.<name>` wrappers. Only the historical wrappers keep a Rust
/// shadow; the current pin declares none, so these names resolve to the root
/// intrinsic without consulting the registry.
const LEGACY_ROOT_WRAPPER_OPERATIONS: &[&str] = &[
    "filter", "map", "flat_map", "sort_by", "take", "drop", "distinct", "union", "count", "first",
    "one", "sum", "min", "max", "every", "exists",
];

/// Whether a captured pin still declares the legacy root wrapper that the
/// registry shadows. A wrapper whose `*_from` helper is a `while` loop is
/// evaluated from its Orna body instead (ORNA-LIB-001).
fn legacy_root_wrapper_is_pinned(prefix: &str, operation: &str, functions: &Functions) -> bool {
    functions.contains_key(&format!("{prefix}{operation}"))
        && !captured_collection_helper_is_loop(functions, &format!("{prefix}{operation}_from"))
}

/// Whether a registered collection binding still evaluates in Rust for a
/// captured pin. `__` leaves are always native. `map` stays native only while
/// its pinned `map_from` is not a loop. Other public operations take their
/// Orna bodies for list inputs; relation inputs are routed before this gate.
fn captured_collection_binding_is_native(operation: &str, functions: &Functions) -> bool {
    operation.starts_with("__")
        || (operation == "map"
            && !captured_collection_helper_is_loop(functions, "std.collection.map_from"))
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
        "sys.Materialization" => Some("sys.Materialization"),
        _ => None,
    }
}

const SYS_MATERIALIZATION_SOURCE: &str = "sys.Materialization";

fn sha256_parts(domain: &str, parts: &[&[u8]]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(domain.as_bytes());
    hash.update([0]);
    for part in parts {
        hash.update(part);
    }
    hash.finalize().into()
}

fn materialization_row(record: &MaterializationRecord) -> Value {
    let count = |value: Option<usize>| {
        Value::Option(value.map(|count| Box::new(Value::Int(BigInt::from(count)))))
    };
    let mut fields = BTreeMap::from([
        ("object".to_owned(), Value::Blob(record.object.to_vec())),
        ("snapshot".to_owned(), Value::Blob(record.snapshot.to_vec())),
        ("digest".to_owned(), Value::Blob(record.digest.to_vec())),
        ("rows".to_owned(), count(Some(record.rows))),
        ("bytes".to_owned(), count(record.bytes)),
        ("valid".to_owned(), Value::Bool(record.valid)),
    ]);
    if let Some(reference) = &record.reference {
        fields.insert("reference".to_owned(), Value::Reference(reference.clone()));
    }
    fields.insert("created".to_owned(), record.created.clone());
    Value::Record(fields)
}

/// Callee used to read the activation clock for `created`. It is never a user
/// annotation; the host effect handler decides whether a clock exists.
fn materialization_clock_callee() -> Expr {
    Expr::Name {
        text: "now".to_owned(),
        span: orna_syntax_v1::SyntaxSpan::new(0, 0),
    }
}

/// Incremental canonical digest of one materialized output. Rows fold in as
/// they stream, so a decision never retains its full output. Encoding stops at
/// the first row with no canonical form; later rows are still counted.
struct OutputDigest {
    digest: Sha256,
    rows: usize,
    bytes: usize,
    valid: bool,
}

impl OutputDigest {
    fn new() -> Self {
        let mut digest = Sha256::new();
        digest.update(b"orna.materialization.digest.v1\0");
        Self {
            digest,
            rows: 0,
            bytes: 0,
            valid: true,
        }
    }

    fn push(&mut self, value: &Value) {
        self.rows = self.rows.saturating_add(1);
        if !self.valid {
            return;
        }
        let encoded = value.canonical_bytes();
        match encoded {
            Ok(encoded) => {
                self.digest.update((encoded.len() as u64).to_be_bytes());
                self.digest.update(&encoded);
                self.bytes = self.bytes.saturating_add(encoded.len());
            }
            Err(_) => self.valid = false,
        }
    }

    /// Returns the digest, the encoded byte count when every row is canonical,
    /// and that validity flag.
    fn finish(self) -> ([u8; 32], Option<usize>, bool) {
        (
            self.digest.finalize().into(),
            self.valid.then_some(self.bytes),
            self.valid,
        )
    }
}

/// Stage identity of a relation plan: its source name, union children, inline
/// source rows, and every stage in order. Read-scope identities are not part
/// of it, so the same logical stages over the same source share one key.
fn relation_stage_digest(plan: &RelationPlan) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"orna.materialization.object.v1\0");
    update_relation_identity(&mut hash, plan);
    hash.finalize().into()
}

fn update_relation_identity(hash: &mut Sha256, plan: &RelationPlan) {
    hash_identity_part(hash, b"source", plan.source.as_bytes());
    if let Some((left, right)) = &plan.source_union {
        hash_identity_part(hash, b"union", &[]);
        update_relation_identity(hash, left);
        update_relation_identity(hash, right);
    }
    if let Some(values) = &plan.source_values {
        hash_identity_part(hash, b"values", format!("{values:?}").as_bytes());
    }
    for stage in &plan.stages {
        hash_identity_part(hash, b"stage", stage.fingerprint().as_bytes());
    }
    hash_identity_part(hash, b"end", &[]);
}

fn hash_identity_part(hash: &mut Sha256, tag: &[u8], payload: &[u8]) {
    hash.update((tag.len() as u64).to_be_bytes());
    hash.update(tag);
    hash.update((payload.len() as u64).to_be_bytes());
    hash.update(payload);
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

/// The dotted path of a plain `a.b.c` name chain, or `None` for any other
/// expression.
fn dotted_name_path(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Name { text, .. } => Some(text.clone()),
        Expr::Field { base, name, .. } => Some(format!("{}.{name}", dotted_name_path(base)?)),
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
fn raw_option_value(value: Option<&Value>) -> Result<Raw, EvaluationError> {
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
    if let (Value::Float(a), Value::Float(b)) = (left, right) {
        // ORNA-FLOAT-007: canonical NaN if either input is NaN; `min` prefers -0.0
        // and `max` prefers +0.0 among zeros, otherwise numeric order decides.
        let values = [*a, *b];
        let bits = if min { float_min(&values) } else { float_max(&values) };
        return bits.map(Value::Float).ok_or_else(|| error("ORNA-EVAL-VALUE"));
    }
    let ordering = compare_values(left, right)?;
    if (ordering.is_le()) == min {
        Ok(left.clone())
    } else {
        Ok(right.clone())
    }
}
fn float_value(value: f64) -> Value {
    Value::Float(if value.is_nan() {
        CANONICAL_NAN_BITS
    } else {
        value.to_bits()
    })
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
    // ORNA-LIT-002; grammar `decimal_integer , exponent`: an exponent numeral
    // without a fraction is exact too, with no fraction digits.
    let (whole, fraction) = match mantissa.split_once('.') {
        Some(parts) => parts,
        None if mantissa.len() < text.len() => (mantissa, ""),
        None => return Err(error("ORNA-EVAL-VALUE")),
    };
    let coefficient = BigInt::parse_bytes(format!("{whole}{fraction}").as_bytes(), 10)
        .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
    let fraction_digits = i64::try_from(fraction.len()).map_err(|_| error("ORNA-EVAL-LIMIT"))?;
    let exponent = exponent
        .checked_sub(fraction_digits)
        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
    Ok((coefficient, BigInt::from(exponent)))
}
fn unescape_string(text: &str) -> Result<String, EvaluationError> {
    orna_syntax_v1::unescape_string_literal(text).map_err(|_| error("ORNA-EVAL-VALUE"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn canonical_causes_keep_catalogue_codes_and_redact_other_codes() {
        let inner = CanonicalErrorValue::new(
            "ORNA-EVAL-DIVIDE-BY-ZERO",
            "private message",
            [],
            BTreeMap::new(),
        )
        .expect("canonical inner error");
        let user =
            CanonicalErrorValue::new("userSuppliedCode", "private message", [], BTreeMap::new())
                .expect("canonical user error");
        let private_orna =
            CanonicalErrorValue::new("ORNA-PRIVATE-TOKEN", "private message", [], BTreeMap::new())
                .expect("canonical ORNA-prefixed user error");
        let outer = CanonicalErrorValue::new(
            "ORNA-EVAL-SURFACE",
            "private message",
            [inner, user, private_orna],
            BTreeMap::new(),
        )
        .expect("canonical outer error");

        let error = EvaluationError::from_canonical(outer);
        let causes = error.diagnostic().causes();
        assert_eq!(error.code(), "ORNA-EVAL-ERROR");
        assert_eq!(causes.len(), 3);
        assert_eq!(causes[0].code(), "ORNA-EVAL-DIVIDE-BY-ZERO");
        assert_eq!(causes[1].code(), "<redacted>");
        assert_eq!(causes[2].code(), "<redacted>");
        assert!(
            causes
                .iter()
                .all(|cause| cause.code() != "ORNA-PRIVATE-TOKEN")
        );
        assert!(causes.iter().all(|cause| cause.message() == "<redacted>"));
    }

    /// `std.collection.map_from` at the historical pin a987024 (recursive).
    const HISTORICAL_MAP_FROM: &str = "fn map_from<T, U>(
    values: [T], transform: fn(T): U, index: Int, end: Int, output: [U]
): [U] =
    if index >= end {
        output
    } else {
        map_from(values, transform, index + 1, end, append(output, transform(values[index])))
    };
";

    /// Loads every function of a standard collection module under `std.collection.`.
    fn collection_functions_for(source: &str) -> Functions {
        let parsed = orna_syntax_v1::parse_module(source);
        assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
        let mut functions = Functions::new();
        for item in parsed.value.items {
            let orna_syntax_v1::Declaration::Function { signature, body } = item.declaration
            else {
                continue;
            };
            functions.insert(
                format!("std.collection.{}", signature.name),
                PureFunction {
                    parameters: signature.parameters,
                    body,
                    environment: Environment::new(),
                },
            );
        }
        functions
    }

    #[test]
    fn captured_collection_map_gate_follows_the_pinned_map_from_shape() {
        // Historical pins keep the native map binding: their body is not a loop.
        assert!(captured_collection_binding_is_native(
            "map",
            &collection_functions_for(HISTORICAL_MAP_FROM)
        ));
        // A pin whose map_from is a while loop shadows native map with its Orna body.
        let looping_map_from = "fn map_from<T, U>(values: [T], transform: fn(T): U, index: Int, end: Int, output: [U]): [U] { let cursor = index; let result = output; while cursor < end { cursor = cursor + 1; }; result }";
        assert!(!captured_collection_binding_is_native(
            "map",
            &collection_functions_for(looping_map_from)
        ));
        // The current pin declares no map_from or map wrapper, so the root intrinsic is native.
        assert!(captured_collection_binding_is_native(
            "map",
            &collection_functions_for(include_str!("../../../stdlib/std/collection.orna"))
        ));
        assert!(captured_collection_binding_is_native("map", &Functions::new()));
        assert!(captured_collection_binding_is_native("__stable_sort", &Functions::new()));
    }

    #[test]
    fn pinned_collection_declares_no_legacy_root_wrappers() {
        let pinned = collection_functions_for(include_str!("../../../stdlib/std/collection.orna"));
        for operation in LEGACY_ROOT_WRAPPER_OPERATIONS {
            assert!(!pinned.contains_key(&format!("std.collection.{operation}")), "{operation}");
            assert!(
                !pinned.contains_key(&format!("std.collection.{operation}_from")),
                "{operation}_from"
            );
        }
        for (operation, helper) in [
            ("chunk", "chunk_from"),
            ("flatten", "flatten_from"),
            ("partition", "partition_from"),
            ("zip", "zip_from"),
            ("pairs", "pairs_from"),
            ("window", "windows_from"),
            ("split_when", "split_when_from"),
        ] {
            assert!(pinned.contains_key(&format!("std.collection.{operation}")), "{operation}");
            assert!(
                captured_collection_helper_is_loop(&pinned, &format!("std.collection.{helper}")),
                "{helper} must be a while loop in the pin"
            );
            assert!(!captured_collection_binding_is_native(operation, &pinned), "{operation}");
        }
    }

    const HISTORICAL_ROOT_WRAPPERS: &str = "fn filter<T>(values: [T], predicate: fn(T): Bool): [T] = filter_from(values, predicate, 0, __list_length(values), []);
fn filter_from<T>(values: [T], predicate: fn(T): Bool, index: Int, end: Int, output: [T]): [T] = if index >= end { output } else { filter_from(values, predicate, index + 1, end, output) };
";

    #[test]
    fn legacy_root_wrapper_shadow_follows_the_captured_pin() {
        let scope = Scope(
            BTreeMap::new(),
            BTreeSet::new(),
            BTreeSet::new(),
            NominalDefinitions::new(),
            BTreeSet::new(),
        );
        let call = orna_syntax_v1::parse_expression("std.collection.filter(values)");
        assert!(call.is_ok(), "{:?}", call.diagnostics);
        // Replay of a pin that still declares the root wrapper keeps the native shadow.
        let historical = collection_functions_for(HISTORICAL_ROOT_WRAPPERS);
        assert!(is_native_collection_binding(
            &call.value,
            Some("std.collection.filter"),
            &scope,
            false,
            true,
            &historical,
        ));
        // The current pin declares no wrapper, so the name is not registered.
        let current = collection_functions_for(include_str!("../../../stdlib/std/collection.orna"));
        assert!(!is_native_collection_binding(
            &call.value,
            Some("std.collection.filter"),
            &scope,
            false,
            true,
            &current,
        ));
        assert!(
            registered_standard_binding(
                &call.value,
                Some("std.collection.filter"),
                &scope,
                false,
                true,
                true,
                &current,
            )
            .is_none()
        );
    }

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
        let mut stub_candidates = 0;
        for (path, source) in orna_standard_sources::reference_standard_sources_v1() {
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
                stub_candidates += 1;
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
                    &Functions::new(),
                );
                #[cfg(feature = "system-host-bindings")]
                let system_host = orna_sys_v1::system_host_operation_registry()
                    .operation(&qualified)
                    .is_some();
                if intrinsic.is_some() || system_host {
                    registered += 1;
                }
            }
        }
        assert_eq!(
            stub_candidates,
            30,
            "pinned std snapshot has 30 explicit host-binding stubs"
        );
        assert!(
            registered >= 4,
            "expected the pinned intrinsic/sys-host subset, got {registered} registered of {stub_candidates} explicit stubs"
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
            calls: 0,
            functions,
            aliases: None,
            session_functions: None,
            repl_bindings: false,
            restrict_function_names: false,
            reject_unhandled_field_calls: false,
            effects: None,
            namespace: None,
            transfer: None,
            materializations: Vec::new(),
            materialization_clock_unavailable: false,
            reuse: CallReuse::default(),
            sys_host: None,
            cancellation: None,
        }
    }

    #[test]
    fn authored_annotation_preserves_format3_owner_context() {
        struct EmptyResolver;

        impl orna_value_v1::BlobResolver for EmptyResolver {
            fn read_range(
                &self,
                _reference: &orna_value_v1::ContentReference,
                _offset: u64,
                _length: u64,
            ) -> orna_value_v1::Result<Vec<u8>> {
                Err(orna_value_v1::Error::ContentUnavailable)
            }
        }

        let database = [0x21; 16];
        let owner_context = orna_value_v1::Format3Context::from_persisted(
            database,
            orna_value_v1::Snapshot::cwd(database, [0x22; 16], 0.into())
                .expect("format-3 snapshot"),
            orna_value_v1::GitHash::Sha1,
            orna_value_v1::NativeOid::from_bytes(&[0x23; 20]).unwrap(),
            orna_value_v1::NativeOid::from_bytes(&[0x24; 20]).unwrap(),
            orna_value_v1::AvailabilityState::Available,
            orna_value_v1::OwnerLifetime::new([0x25; 16]),
            orna_value_v1::Format3Quota::new(1024, 1 << 20).unwrap(),
            Arc::new(EmptyResolver),
        )
        .expect("format-3 owner context");
        let reference = owner_context
            .reference(
                orna_value_v1::ContentIdentity::from_bytes(b"protected payload"),
                orna_value_v1::NativeOid::from_bytes(&[0x26; 20]).unwrap(),
            )
            .expect("owner-scoped Blob reference");
        let blob = orna_value_v1::Blob::from_reference(reference).expect("owner-scoped Blob");
        let source =
            include_str!("../../orna-cli-v1/tests/fixtures/media/import-song-annotation.orna")
                .replace(
                    "sys.blob.capture_file(__MEDIA_ROOT__, \"tone.wav\", 65536)",
                    "blob",
                );
        let parsed = parse_expression(&source);
        assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);

        for format in [ValueFormat::Rov3, ValueFormat::Sov3] {
            let source_value = ContextValue::from_blob(&blob, format)
                .expect("encode owner-scoped Blob in source format");
            let functions = Functions::new();
            let mut context = test_context(&functions);
            let mut scope = Scope::from_context_environment(
                &BTreeMap::from([(String::from("blob"), source_value)]),
                &mut context,
            )
            .expect("retain source ContextValue in the evaluator environment");
            let result = context
                .evaluate(&parsed.value, &mut scope, 0)
                .unwrap_or_else(|error| panic!("authored annotation call: {}", error.code()));
            let Value::ContextualBlob(annotated) = result else {
                panic!("annotation result must retain its contextual Blob");
            };
            assert_eq!(annotated.format(), format);
            assert!(annotated.context().is_some());
            let annotated_blob = annotated.blob().expect("decode annotated Blob");
            assert_eq!(annotated_blob.content_identity(), blob.content_identity());
            assert_eq!(annotated_blob.media_type(), "audio/mp4");
            assert_eq!(annotated_blob.suffix(), Some("m4b"));
            assert!(!annotated_blob.is_hydrated());
        }
        let ovb2_blob = orna_value_v1::Blob::from_bytes_with_annotation(
            b"payload".to_vec(),
            "audio/mpeg",
            None,
        )
        .expect("create OVB-2 Blob");
        let top_level = ContextValue::from_blob(&ovb2_blob, ValueFormat::Ovb2)
            .expect("encode top-level OVB-2 Blob");
        let nested = ContextValue::new(
            ValueFormat::Ovb2,
            Raw::Map(vec![(Raw::Text("blob".into()), top_level.raw().clone())]),
        )
        .expect("encode OVB-2 record with nested Blob");
        let equality = parse_expression("record.blob == blob");
        assert!(equality.is_ok(), "{:?}", equality.diagnostics);
        let functions = Functions::new();
        let mut context = test_context(&functions);
        let mut scope = Scope::from_context_environment(
            &BTreeMap::from([
                (String::from("blob"), top_level),
                (String::from("record"), nested),
            ]),
            &mut context,
        )
        .expect("decode OVB-2 equality environment");
        assert_eq!(
            context
                .evaluate(&equality.value, &mut scope, 0)
                .expect("evaluate nested Blob equality"),
            Value::Bool(true),
        );
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
                    Ok(Value::list(context.collect_relation_values(&plan, 0)?))
                }
                Value::Tuple(parts) => Ok(Value::Tuple(
                    parts
                        .into_iter()
                        .map(|part| materialize(context, part))
                        .collect::<Result<Vec<_>, _>>()?,
                )),
                Value::List(values) => Ok(Value::list(
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
            Value::list(vec![
                Value::Tuple(vec![Value::Int(BigInt::from(1)), Value::list(ints(&[31]))]),
                Value::Tuple(vec![
                    Value::Int(BigInt::from(2)),
                    Value::list(ints(&[12, 22])),
                ]),
                Value::Tuple(vec![
                    Value::Int(BigInt::from(3)),
                    Value::list(ints(&[13, 33])),
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
            Value::Tuple(vec![Value::list(ints(&[2, 4])), Value::list(ints(&[3, 1]))])
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
            Value::list(vec![Value::list(ints(&[1, 2])), Value::list(ints(&[3]))])
        );
        assert_eq!(
            evaluate_relation_collection_fixture(
                include_str!("../tests/fixtures/relation-flatten-result-0re2w.orna"),
                &[(
                    "rows",
                    vec![Value::list(ints(&[1, 2])), Value::list(ints(&[3]))]
                )],
            )
            .unwrap(),
            Value::list(ints(&[1, 2, 3]))
        );
        let left = ints(&[1, 2]);
        let right = ["a", "b", "c"]
            .into_iter()
            .map(|value| Value::String(value.into()))
            .collect::<Vec<_>>();
        let zipped = Value::list(vec![
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
            Value::list(ints(&[3, 1, 2]))
        );
        assert_eq!(
            evaluate_relation_collection_fixture(
                include_str!("../tests/fixtures/relation-split-when-result-0re2w.orna"),
                &[("rows", ints(&[1, 2, 3, 4]))],
            )
            .unwrap(),
            Value::list(vec![
                Value::list(ints(&[1])),
                Value::list(ints(&[2, 3])),
                Value::list(ints(&[4])),
            ])
        );
        assert_eq!(
            evaluate_relation_collection_fixture(
                include_str!("../tests/fixtures/relation-rank-result-0re2w.orna"),
                &[("rows", ints(&[3, 1, 2, 1]))],
            )
            .unwrap(),
            Value::list(vec![
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
            Value::list(vec![Value::Tuple(vec![
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
            Value::list(vec![
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
                Value::list(ints(&[0, 2])),
                Value::Int(BigInt::from(1)),
                Value::Int(BigInt::from(1)),
                Value::list(ints(&[1, 1])),
            ])
        );

        let empty = evaluate_relation_collection_fixture(
            include_str!("../tests/fixtures/relation-statistics-empty-6c10u.orna"),
            &[("rows", Vec::new())],
        )
        .expect("empty aggregates use their documented identities");
        assert_eq!(
            empty,
            Value::list(vec![
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Int(BigInt::from(0)),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::list(Vec::new()),
                Value::Null,
                Value::Null,
                Value::list(ints(&[0])),
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
            Value::list(vec![
                Value::Int(BigInt::from(1)),
                Value::list(vec![Value::Tuple(vec![
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
    fn context_named_invocation_preserves_nested_blob_through_source_calls() {
        struct ContextProbe {
            observed: Option<ContextValue>,
        }

        impl EffectHandler for ContextProbe {
            fn handle(
                &mut self,
                _callee: &Expr,
                _arguments: &[CanonicalValue],
            ) -> Result<Option<CanonicalValue>, EvaluationError> {
                Err(error("ORNA-EVAL-UNSUPPORTED"))
            }
        }

        impl ContextValueEffectHandler for ContextProbe {
            fn handle_context(
                &mut self,
                _callee: &Expr,
                arguments: &[ContextValue],
            ) -> Result<Option<ContextValue>, EvaluationError> {
                let [argument] = arguments else {
                    return Err(error("ORNA-EVAL-ARGUMENT"));
                };
                self.observed = Some(argument.clone());
                Ok(Some(argument.clone()))
            }
        }

        let parsed = orna_syntax_v1::parse_module(
            "fn relay(value: Blob) = sys.test.persist(value); fn entry(value: Blob) = relay(value);",
        );
        assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
        let functions = Functions::from([
            (
                String::from("relay"),
                PureFunction {
                    parameters: match &parsed.value.items[0].declaration {
                        orna_syntax_v1::Declaration::Function { signature, .. } => {
                            signature.parameters.clone()
                        }
                        _ => panic!("function expected"),
                    },
                    body: match &parsed.value.items[0].declaration {
                        orna_syntax_v1::Declaration::Function { body, .. } => body.clone(),
                        _ => panic!("function expected"),
                    },
                    environment: Environment::new(),
                },
            ),
            (
                String::from("entry"),
                PureFunction {
                    parameters: match &parsed.value.items[1].declaration {
                        orna_syntax_v1::Declaration::Function { signature, .. } => {
                            signature.parameters.clone()
                        }
                        _ => panic!("function expected"),
                    },
                    body: match &parsed.value.items[1].declaration {
                        orna_syntax_v1::Declaration::Function { body, .. } => body.clone(),
                        _ => panic!("function expected"),
                    },
                    environment: Environment::new(),
                },
            ),
        ]);
        let blob = Raw::Tag(
            OVB2_BLOB_TAG,
            Box::new(Raw::Array(vec![
                Raw::Bytes(b"image".to_vec()),
                Raw::Text("image/png".to_owned()),
                Raw::Null,
            ])),
        );
        let tuple = Raw::Tag(60015, Box::new(Raw::Array(vec![blob])));
        let value = ContextValue::new(ValueFormat::Ovb2, tuple.clone())
            .expect("well-formed nested OVB-2 Blob tuple");
        let arguments = BTreeMap::from([(String::from("value"), value)]);
        let mut effects = ContextProbe { observed: None };

        let mut budget = StepBudget::new(Limits::default().max_steps);
        let result = invoke_named_with_context_effects(
            "entry",
            &functions,
            &arguments,
            Limits::default(),
            &mut effects,
            &mut budget,
        )
        .expect("nested source calls preserve ContextValue effect arguments");

        assert_eq!(result.format(), ValueFormat::Ovb2);
        assert_eq!(result.raw(), &tuple);
        assert_eq!(effects.observed.expect("effect invoked").raw(), &tuple);
    }

    #[test]
    fn context_named_invocation_adopts_effect_result_profile() {
        struct ProfileChange;

        impl EffectHandler for ProfileChange {
            fn handle(
                &mut self,
                _callee: &Expr,
                _arguments: &[CanonicalValue],
            ) -> Result<Option<CanonicalValue>, EvaluationError> {
                Err(error("ORNA-EVAL-UNSUPPORTED"))
            }
        }

        impl ContextValueEffectHandler for ProfileChange {
            fn handle_context(
                &mut self,
                _callee: &Expr,
                arguments: &[ContextValue],
            ) -> Result<Option<ContextValue>, EvaluationError> {
                assert_eq!(arguments.len(), 1);
                assert_eq!(arguments[0].format(), ValueFormat::Ovb1);
                let blob = Raw::Tag(
                    OVB2_BLOB_TAG,
                    Box::new(Raw::Array(vec![
                        Raw::Bytes(b"captured".to_vec()),
                        Raw::Text("image/png".to_owned()),
                        Raw::Null,
                    ])),
                );
                Ok(Some(
                    ContextValue::new(ValueFormat::Ovb2, blob)
                        .expect("captured Blob is valid OVB-2"),
                ))
            }
        }

        let parsed = orna_syntax_v1::parse_module("fn entry(path: Str) = sys.test.capture(path);");
        assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
        let orna_syntax_v1::Declaration::Function { signature, body } =
            &parsed.value.items[0].declaration
        else {
            panic!("function expected")
        };
        let functions = Functions::from([(
            String::from("entry"),
            PureFunction {
                parameters: signature.parameters.clone(),
                body: body.clone(),
                environment: Environment::new(),
            },
        )]);
        let arguments = BTreeMap::from([(
            String::from("path"),
            ContextValue::new(ValueFormat::Ovb1, Raw::Text("image.png".to_owned()))
                .expect("path is valid OVB-1"),
        )]);

        let mut budget = StepBudget::new(Limits::default().max_steps);
        let result = invoke_named_with_context_effects(
            "entry",
            &functions,
            &arguments,
            Limits::default(),
            &mut ProfileChange,
            &mut budget,
        )
        .expect("capture result profile is preserved");

        assert_eq!(result.format(), ValueFormat::Ovb2);
        assert!(matches!(result.raw(), Raw::Tag(OVB2_BLOB_TAG, _)));
    }

    #[test]
    fn context_named_invocation_adopts_effect_result_owner_context() {
        struct EmptyResolver;

        impl orna_value_v1::BlobResolver for EmptyResolver {
            fn read_range(
                &self,
                _reference: &orna_value_v1::ContentReference,
                _offset: u64,
                _length: u64,
            ) -> orna_value_v1::Result<Vec<u8>> {
                Err(orna_value_v1::Error::ContentUnavailable)
            }
        }

        struct ReplaceAndConsume {
            returned: ContextValue,
            page_row: ContextValue,
            consumed_context: Option<[u8; 16]>,
            page_reads: usize,
        }

        impl EffectHandler for ReplaceAndConsume {
            fn handle(
                &mut self,
                _callee: &Expr,
                _arguments: &[CanonicalValue],
            ) -> Result<Option<CanonicalValue>, EvaluationError> {
                Err(error("ORNA-EVAL-UNSUPPORTED"))
            }
            fn validate_relation_source(&mut self, source: &str) -> Result<(), EvaluationError> {
                assert_eq!(source, "sys.Storage");
                Ok(())
            }

        }

        impl ContextValueEffectHandler for ReplaceAndConsume {
            fn handle_context(
                &mut self,
                callee: &Expr,
                arguments: &[ContextValue],
            ) -> Result<Option<ContextValue>, EvaluationError> {
                assert_eq!(arguments.len(), 1);
                match function_name(callee).as_deref() {
                    Some("sys.test.replace_blob") => {
                        assert_eq!(arguments[0].format(), ValueFormat::Rov3);
                        Ok(Some(self.returned.clone()))
                    }
                    Some("sys.test.consume_blob") => {
                        let owner = arguments[0]
                            .context()
                            .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                        self.consumed_context = Some(owner.database_uuid());
                        Ok(Some(
                            ContextValue::new(ValueFormat::Ovb1, Raw::Bool(true))
                                .map_err(|_| error("ORNA-EVAL-VALUE"))?,
                        ))
                    }
                    _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
                }
            }

            fn scan_context_relation_page_scoped(
                &mut self,
                source: &str,
                _scope: RelationReadScope,
                after: Option<&[u8]>,
                limit: usize,
                budget: &mut StepBudget,
            ) -> Result<Option<ContextValueRelationPage>, EvaluationError> {
                assert_eq!(source, "sys.Storage");
                assert!(after.is_none(), "the fixture source contains one row");
                assert_eq!(limit, 1);
                budget.debit(1)?;
                self.page_reads += 1;
                Ok(Some(ContextValueRelationPage {
                    rows: vec![self.page_row.clone()],
                    next: None,
                }))
            }
        }

        let context = |database: [u8; 16], base: u8| {
            Format3Context::from_persisted(
                database,
                orna_value_v1::Snapshot::cwd(database, [base; 16], 0.into())
                    .expect("format-3 snapshot"),
                orna_value_v1::GitHash::Sha1,
                orna_value_v1::NativeOid::from_bytes(&[base + 1; 20]).unwrap(),
                orna_value_v1::NativeOid::from_bytes(&[base + 2; 20]).unwrap(),
                orna_value_v1::AvailabilityState::Available,
                orna_value_v1::OwnerLifetime::new([base + 3; 16]),
                orna_value_v1::Format3Quota::new(1024, 1 << 20).unwrap(),
                Arc::new(EmptyResolver),
            )
            .expect("format-3 owner context")
        };
        let source_context = context([0x31; 16], 0x32);
        let result_context = context([0x41; 16], 0x42);
        let source_blob = orna_value_v1::Blob::from_reference(
            source_context
                .reference(
                    orna_value_v1::ContentIdentity::from_bytes(b"source"),
                    orna_value_v1::NativeOid::from_bytes(&[0x37; 20]).unwrap(),
                )
                .unwrap(),
        )
        .unwrap();
        let indexed_row = ContextValue::from_blob(
            &orna_value_v1::Blob::from_reference(
                result_context
                    .reference(
                        orna_value_v1::ContentIdentity::from_bytes(b"replacement"),
                        orna_value_v1::NativeOid::from_bytes(&[0x47; 20]).unwrap(),
                    )
                    .unwrap(),
            )
            .unwrap(),
            ValueFormat::Rov3,
        )
        .unwrap();
        let returned = ContextValue::from_blob(&source_blob, ValueFormat::Rov3).unwrap();
        let page_row = indexed_row.clone();
        let parsed = orna_syntax_v1::parse_module(include_str!(
            "../tests/fixtures/context-result-owner.orna"
        ));
        assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
        let orna_syntax_v1::Declaration::Function { signature, body } =
            &parsed.value.items[0].declaration
        else {
            panic!("function expected")
        };
        let functions = Functions::from([(
            String::from("entry"),
            PureFunction {
                parameters: signature.parameters.clone(),
                body: body.clone(),
                environment: Environment::new(),
            },
        )]);
        let arguments = BTreeMap::from([(
            String::from("blob"),
            indexed_row.clone(),
        )]);

        let mut budget = StepBudget::new(Limits::default().max_steps);
        let mut effects = ReplaceAndConsume {
            returned,
            page_row,
            consumed_context: None,
            page_reads: 0,
        };
        let result = invoke_named_with_context_effects(
            "entry",
            &functions,
            &arguments,
            Limits::default(),
            &mut effects,
            &mut budget,
        )
        .unwrap_or_else(|error| panic!("source consumer failed: {}", error.code()));

        assert_eq!(result.format(), ValueFormat::Rov3);
        assert_eq!(result.context().unwrap().database_uuid(), [0x41; 16]);
        assert_eq!(result.raw(), &Raw::Bool(true));
        assert_eq!(effects.consumed_context, Some([0x41; 16]));
        assert_eq!(effects.page_reads, 1);
    }

    #[test]
    fn context_named_invocation_preserves_owner_context_after_ovb2_effects() {
        struct EmptyResolver;

        impl orna_value_v1::BlobResolver for EmptyResolver {
            fn read_range(
                &self,
                _reference: &orna_value_v1::ContentReference,
                _offset: u64,
                _length: u64,
            ) -> orna_value_v1::Result<Vec<u8>> {
                Err(orna_value_v1::Error::ContentUnavailable)
            }
        }

        struct ProfileEffects {
            calls: usize,
            persisted: Option<ContextValue>,
        }

        impl EffectHandler for ProfileEffects {
            fn handle(
                &mut self,
                _callee: &Expr,
                _arguments: &[CanonicalValue],
            ) -> Result<Option<CanonicalValue>, EvaluationError> {
                Err(error("ORNA-EVAL-UNSUPPORTED"))
            }
        }

        impl ContextValueEffectHandler for ProfileEffects {
            fn handle_context(
                &mut self,
                _callee: &Expr,
                arguments: &[ContextValue],
            ) -> Result<Option<ContextValue>, EvaluationError> {
                let [argument] = arguments else {
                    return Err(error("ORNA-EVAL-ARGUMENT"));
                };
                match self.calls {
                    0 => {
                        assert_eq!(argument.format(), ValueFormat::Sov3);
                        self.calls = 1;
                        Ok(Some(
                            ContextValue::new(ValueFormat::Ovb2, Raw::Text("echo".into()))
                                .expect("OVB-2 effect result"),
                        ))
                    }
                    1 => {
                        assert_eq!(argument.format(), ValueFormat::Sov3);
                        self.persisted = Some(argument.clone());
                        self.calls = 2;
                        Ok(Some(
                            ContextValue::new(ValueFormat::Ovb2, Raw::Text("saved".into()))
                                .expect("OVB-2 effect result"),
                        ))
                    }
                    _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
                }
            }
            fn handle_context_with_budget(
                &mut self,
                callee: &Expr,
                arguments: &[ContextValue],
                budget: &mut StepBudget,
            ) -> Result<Option<ContextValue>, EvaluationError> {
                budget.debit(1)?;
                self.handle_context(callee, arguments)
            }
        }

        let database = [0x21; 16];
        let context = Format3Context::from_persisted(
            database,
            orna_value_v1::Snapshot::cwd(database, [0x22; 16], 0.into())
                .expect("format-3 snapshot"),
            orna_value_v1::GitHash::Sha1,
            orna_value_v1::NativeOid::from_bytes(&[0x23; 20]).unwrap(),
            orna_value_v1::NativeOid::from_bytes(&[0x24; 20]).unwrap(),
            orna_value_v1::AvailabilityState::Available,
            orna_value_v1::OwnerLifetime::new([0x25; 16]),
            orna_value_v1::Format3Quota::new(1024, 1 << 20).unwrap(),
            Arc::new(EmptyResolver),
        )
        .expect("format-3 owner context");
        let identity = orna_value_v1::ContentIdentity::from_bytes(b"protected payload");
        let reference = context
            .reference(
                identity,
                orna_value_v1::NativeOid::from_bytes(&[0x26; 20]).unwrap(),
            )
            .expect("owner-scoped Blob reference");
        let blob = orna_value_v1::Blob::from_reference(reference)
            .expect("owner-scoped Blob reference");
        let blob_value = ContextValue::from_blob(&blob, ValueFormat::Sov3)
            .expect("encode protected Blob in SOV-3")
            .with_blob_annotation("image/png", Some("png"))
            .expect("canonicalize authored PNG annotation in SOV-3");
        let parsed = orna_syntax_v1::parse_module(
            "fn entry(blob: Blob) = (sys.test.persist((sys.test.echo(\"ping\"), blob)), blob);",
        );
        assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
        let orna_syntax_v1::Declaration::Function { signature, body } =
            &parsed.value.items[0].declaration
        else {
            panic!("function expected")
        };
        let functions = Functions::from([(
            String::from("entry"),
            PureFunction {
                parameters: signature.parameters.clone(),
                body: body.clone(),
                environment: Environment::new(),
            },
        )]);
        let arguments = BTreeMap::from([(String::from("blob"), blob_value.clone())]);
        let mut effects = ProfileEffects {
            calls: 0,
            persisted: None,
        };

        let mut budget = StepBudget::new(Limits::default().max_steps);
        let initial_budget = budget.remaining();
        let result = invoke_named_with_context_effects(
            "entry",
            &functions,
            &arguments,
            Limits::default(),
            &mut effects,
            &mut budget,
        )
        .expect("nested effect and tuple retain the source owner context");

        let persisted = effects.persisted.expect("tuple effect was called");
        assert_eq!(persisted.format(), ValueFormat::Sov3);
        assert!(persisted.context().is_some());
        assert_eq!(effects.calls, 2);
        assert!(budget.remaining() < initial_budget);
        assert_eq!(result.format(), ValueFormat::Sov3);
        assert!(result.context().is_some());
        let Raw::Tag(60015, result_tuple) = result.raw() else {
            panic!("expected tuple result")
        };
        let Raw::Array(result_fields) = result_tuple.as_ref() else {
            panic!("expected tuple fields")
        };
        assert_eq!(result_fields[0], Raw::Text("saved".into()));
        assert_eq!(result_fields[1], blob_value.raw().clone());
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
    fn false_executable_assertion_carries_spec_code() {
        let failure = evaluate_expression(
            "if true { assert false; 1 }",
            &Environment::new(),
            Limits::default(),
        )
        .unwrap_err();

        assert_eq!(failure.diagnostic().code(), "ORNA-A091-008");
        assert_eq!(failure.code(), "ORNA-A091-008");
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
                Value::list(vec![Value::Error(failure.clone())]),
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
            calls: 0,
            functions: &functions,
            aliases: None,
            session_functions: None,
            repl_bindings: false,
            restrict_function_names: false,
            reject_unhandled_field_calls: false,
            effects: None,
            namespace: None,
            transfer: None,
            materializations: Vec::new(),
            materialization_clock_unavailable: false,
            reuse: CallReuse::default(),
            sys_host: None,
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
            calls: 0,
            functions: &functions,
            aliases: None,
            session_functions: None,
            repl_bindings: false,
            restrict_function_names: false,
            reject_unhandled_field_calls: false,
            effects: Some(effects),
            namespace: None,
            transfer: None,
            materializations: Vec::new(),
            materialization_clock_unavailable: false,
            reuse: CallReuse::default(),
            sys_host: None,
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
            calls: 0,
            functions: &functions,
            aliases: None,
            session_functions: None,
            repl_bindings: false,
            restrict_function_names: false,
            reject_unhandled_field_calls: false,
            effects: Some(effects),
            namespace: None,
            transfer: None,
            materializations: Vec::new(),
            materialization_clock_unavailable: false,
            reuse: CallReuse::default(),
            sys_host: None,
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
            calls: 0,
            functions: &functions,
            aliases: None,
            session_functions: None,
            repl_bindings: true,
            restrict_function_names: true,
            reject_unhandled_field_calls: true,
            effects: Some(&mut effects),
            namespace: None,
            transfer: None,
            materializations: Vec::new(),
            materialization_clock_unavailable: false,
            reuse: CallReuse::default(),
            sys_host: None,
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
            calls: 0,
            functions: &functions,
            aliases: None,
            session_functions: None,
            repl_bindings: true,
            restrict_function_names: true,
            reject_unhandled_field_calls: true,
            effects: Some(&mut effects),
            namespace: None,
            transfer: None,
            materializations: Vec::new(),
            materialization_clock_unavailable: false,
            reuse: CallReuse::default(),
            sys_host: None,
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

    /// ORNA-ORDER-004: filter-fusion optimization retains callback order and does not cross a map.
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
            calls: 0,
            functions: &functions,
            aliases: None,
            session_functions: None,
            repl_bindings: true,
            restrict_function_names: true,
            reject_unhandled_field_calls: true,
            effects: Some(&mut effects),
            namespace: None,
            transfer: None,
            materializations: Vec::new(),
            materialization_clock_unavailable: false,
            reuse: CallReuse::default(),
            sys_host: None,
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
            calls: 0,
            functions: &functions,
            aliases: None,
            session_functions: None,
            repl_bindings: true,
            restrict_function_names: true,
            reject_unhandled_field_calls: true,
            effects: Some(&mut effects),
            namespace: None,
            transfer: None,
            materializations: Vec::new(),
            materialization_clock_unavailable: false,
            reuse: CallReuse::default(),
            sys_host: None,
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
            calls: 0,
            functions: &functions,
            aliases: None,
            session_functions: None,
            repl_bindings: true,
            restrict_function_names: true,
            reject_unhandled_field_calls: true,
            effects: None,
            namespace: None,
            transfer: None,
            materializations: Vec::new(),
            materialization_clock_unavailable: false,
            reuse: CallReuse::default(),
            sys_host: None,
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
                    Value::Record(BTreeMap::from([("content".into(), Value::list(children))])),
                ),
                ("actions".into(), Value::Record(BTreeMap::new())),
            ]))
        }

        let valid = node(Vec::new());
        assert!(ui::is_ui_presentation_node(&valid, 0, 1).expect("shallow node depth"));

        let nested = node(vec![valid.clone()]);
        assert!(ui::is_ui_presentation_node(&nested, 0, 1).expect("child at depth one"));
        assert_eq!(
            ui::is_ui_presentation_node(&nested, 0, 0)
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
        assert!(
            !ui::is_ui_presentation_node(&parent, 0, 2).expect("invalid child is not too deep")
        );
    }

    #[test]
    fn presentation_optional_types_preserve_nested_type_paths() {
        assert!(ui::ui_property_type_matches(
            "std.option<std.option<app.Token>>",
            &Value::Option(None)
        ));
        assert!(!ui::ui_property_type_matches(
            "std.option<>",
            &Value::Option(None)
        ));
        assert!(!ui::ui_property_type_matches(
            "std.option<std.option<>>",
            &Value::Null
        ));
        assert!(!ui::ui_property_type_matches(
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

#[cfg(test)]
mod table_reference_value_tests {
    use super::*;

    fn context(functions: &Functions) -> Context<'_, '_> {
        Context {
            limits: Limits::default(),
            steps: 0,
            calls: 0,
            functions,
            aliases: None,
            session_functions: None,
            repl_bindings: false,
            restrict_function_names: false,
            reject_unhandled_field_calls: false,
            effects: None,
            namespace: None,
            transfer: None,
            materializations: Vec::new(),
            materialization_clock_unavailable: false,
            reuse: CallReuse::default(),
            sys_host: None,
            cancellation: None,
        }
    }

    #[test]
    fn stored_table_reference_is_an_equality_only_identity_value() {
        let functions = Functions::new();
        let mut context = context(&functions);
        let reference = orna_value_v1::Value::table_reference(
            [1; 16],
            [2; 16],
            Raw::Text("ada".into()),
        )
        .unwrap();

        let value = Value::from_canonical(&reference, &mut context, 0).unwrap();
        assert!(matches!(&value, Value::TableReference(identity) if identity == &reference));
        assert_eq!(Value::from_raw(reference.raw(), &mut context, 0).unwrap(), value);

        let same = orna_value_v1::Value::table_reference([1; 16], [2; 16], Raw::Text("ada".into()))
            .unwrap();
        let same = Value::from_canonical(&same, &mut context, 0).unwrap();
        assert_eq!(value, same);
        let other_key =
            orna_value_v1::Value::table_reference([1; 16], [2; 16], Raw::Text("grace".into()))
                .unwrap();
        let other_key = Value::from_canonical(&other_key, &mut context, 0).unwrap();
        assert_ne!(value, other_key);
        // References are equality-only: ordering is rejected with a typed error.
        assert!(compare_values(&value, &other_key).is_err());
        assert!(compare_values(&value, &same).is_err());

        let other_table =
            orna_value_v1::Value::table_reference([1; 16], [3; 16], Raw::Text("ada".into()))
                .unwrap();
        let other_table = Value::from_canonical(&other_table, &mut context, 0).unwrap();
        assert_ne!(value, other_table);
    }
}

#[cfg(test)]
mod base64_accept_table {
    use super::decode_base64;

    #[test]
    fn evaluator_base64_follows_the_workspace_strict_profile() {
        let cases: &[(&str, bool)] = &[
            ("", true),
            ("Zm8=", true),
            ("+/8=", true),
            ("-_8=", false),
            ("Zm8", false),
            ("Zm9v=", false),
            ("Zh==", false),
            ("Zm8= ", false),
            ("Zg==Zm9v", false),
        ];
        for &(input, accepted) in cases {
            assert_eq!(decode_base64(input).is_ok(), accepted, "input {input:?}");
        }
    }
}

#[cfg(test)]
mod blob_equality_tests {
    use super::*;

    struct PayloadResolver {
        payload: Vec<u8>,
        reads: std::sync::atomic::AtomicUsize,
    }

    impl orna_value_v1::BlobResolver for PayloadResolver {
        fn read_range(
            &self,
            _reference: &orna_value_v1::ContentReference,
            offset: u64,
            length: u64,
        ) -> orna_value_v1::Result<Vec<u8>> {
            self.reads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let start = usize::try_from(offset).map_err(|_| orna_value_v1::Error::InvalidRange)?;
            let length = usize::try_from(length).map_err(|_| orna_value_v1::Error::InvalidRange)?;
            let end = start
                .checked_add(length)
                .ok_or(orna_value_v1::Error::InvalidRange)?;
            self.payload
                .get(start..end)
                .map(<[u8]>::to_vec)
                .ok_or(orna_value_v1::Error::InvalidRange)
        }
    }

    fn annotated_ovb2(blob: &orna_value_v1::Blob) -> Value {
        Value::AnnotatedBlob(
            ContextValue::from_blob(blob, ValueFormat::Ovb2)
                .unwrap()
                .raw()
                .clone(),
        )
    }

    #[test]
    fn blob_equality_ignores_representation_and_respects_annotation() {
        let payload = b"payload";
        let database = [1; 16];
        let probe = std::sync::Arc::new(PayloadResolver {
            payload: payload.to_vec(),
            reads: std::sync::atomic::AtomicUsize::new(0),
        });
        let resolver: std::sync::Arc<dyn orna_value_v1::BlobResolver> = probe.clone();
        let context = Format3Context::from_persisted(
            database,
            orna_value_v1::Snapshot::cwd(database, [2; 16], 0.into()).unwrap(),
            orna_value_v1::GitHash::Sha1,
            orna_value_v1::NativeOid::from_bytes(&[9; 20]).unwrap(),
            orna_value_v1::NativeOid::from_bytes(&[8; 20]).unwrap(),
            orna_value_v1::AvailabilityState::Available,
            orna_value_v1::OwnerLifetime::new([3; 16]),
            orna_value_v1::Format3Quota::new(8 * 1024 * 1024, 1_u64 << 62).unwrap(),
            resolver,
        )
        .unwrap();
        let reference = context
            .reference(
                orna_value_v1::ContentIdentity::from_bytes(payload),
                orna_value_v1::NativeOid::from_bytes(&[7; 20]).unwrap(),
            )
            .unwrap();
        let reads_before = probe.reads.load(std::sync::atomic::Ordering::SeqCst);
        let rov3_blob = orna_value_v1::Blob::from_reference_with_annotation(
            reference,
            "application/octet-stream",
            None,
        )
        .unwrap();
        let rov3 = Value::ContextualBlob(
            ContextValue::from_blob(&rov3_blob, ValueFormat::Rov3).unwrap(),
        );
        let ovb2_blob = orna_value_v1::Blob::from_bytes_with_annotation(
            payload.to_vec(),
            "application/octet-stream",
            None,
        )
        .unwrap();
        let ovb2 = annotated_ovb2(&ovb2_blob);

        // Equal bytes and annotation compare exactly, including when nested in
        // containers. Two independently built OVB-2 values share no Arc, so this
        // exercises the exact content path rather than pointer identity.
        let ovb2_again = annotated_ovb2(&orna_value_v1::Blob::from_bytes_with_annotation(
            payload.to_vec(),
            "application/octet-stream",
            None,
        )
        .unwrap());
        assert!(blob_aware_eq(&ovb2, &ovb2_again).unwrap());
        assert!(
            blob_aware_eq(
                &Value::Tuple(vec![ovb2.clone()]),
                &Value::Tuple(vec![ovb2_again.clone()]),
            )
            .unwrap()
        );
        assert!(
            blob_aware_eq(
                &Value::Option(Some(Box::new(ovb2.clone()))),
                &Value::Option(Some(Box::new(ovb2_again.clone()))),
            )
            .unwrap()
        );
        assert!(
            blob_aware_eq(
                &Value::List(List::from(vec![ovb2.clone()])),
                &Value::List(List::from(vec![ovb2_again.clone()])),
            )
            .unwrap()
        );

        // A descriptor-only ROV-3 value carries a descriptor but no admitted
        // bytes in this evaluation, so it has no shared witness with OVB-2
        // content. ORNA-BLOB-005 permits explicit unavailability here.
        assert!(blob_aware_eq(&rov3, &ovb2).is_err());
        assert!(blob_aware_eq(&ovb2, &rov3).is_err());
        // Operator `==` path: a descriptor-only ROV-3 value equals itself through
        // its shared descriptor witness (ORNA-BLOB-005).
        assert!(blob_aware_eq(&rov3, &rov3).unwrap());

        // The same bytes under a different annotation are not equal.
        let annotated_blob = orna_value_v1::Blob::from_bytes_with_annotation(
            payload.to_vec(),
            "audio/mp4",
            Some("m4b"),
        )
        .unwrap();
        assert!(!blob_aware_eq(&rov3, &annotated_ovb2(&annotated_blob)).unwrap());

        // Different bytes under the same annotation are not equal.
        let other_blob = orna_value_v1::Blob::from_bytes_with_annotation(
            b"other!!".to_vec(),
            "application/octet-stream",
            None,
        )
        .unwrap();
        assert!(!blob_aware_eq(&rov3, &annotated_ovb2(&other_blob)).unwrap());

        // Non-Blob values keep ordinary equality.
        assert!(
            blob_aware_eq(
                &Value::String("a".to_owned()),
                &Value::String("a".to_owned()),
            )
            .unwrap()
        );
        // Equality over descriptor-only ROV-3 never resolves bytes.
        assert_eq!(
            probe.reads.load(std::sync::atomic::Ordering::SeqCst),
            reads_before
        );
    }

    #[test]
    fn blob_self_equality_and_nested_exact_content() {
        let payload = b"payload";
        let make = |bytes: &[u8]| {
            annotated_ovb2(
                &orna_value_v1::Blob::from_bytes_with_annotation(
                    bytes.to_vec(),
                    "application/octet-stream",
                    None,
                )
                .unwrap(),
            )
        };
        let left = make(payload);
        // Self-equality of an OVB-2 value holds.
        assert!(blob_aware_eq(&left, &left).unwrap());

        // Independently built, equal-content OVB-2 values nested in each
        // compound shape compare equal by exact content (no shared Arc).
        let enum_of = |blob: Value| Value::Enum {
            type_id: Raw::Text("E".into()),
            variant_id: Raw::Text("V".into()),
            payload: Some(Box::new(blob)),
        };
        let record_of = |blob: Value| Value::NominalRecord {
            type_id: Raw::Text("R".into()),
            fields: vec![(Raw::Text("b".into()), blob)],
        };
        let stream_of = |blob: Value| Value::Stream {
            values: vec![blob],
            source_label: "s".to_owned(),
            source_digest: [0; 32],
            position: 0,
            provider: None,
        };
        assert!(blob_aware_eq(&enum_of(left.clone()), &enum_of(make(payload))).unwrap());
        assert!(blob_aware_eq(&record_of(left.clone()), &record_of(make(payload))).unwrap());
        assert!(blob_aware_eq(&stream_of(left.clone()), &stream_of(make(payload))).unwrap());

        // Equal length, different bytes: not equal in any nested shape.
        let other = make(b"paylaod");
        assert_eq!(payload.len(), b"paylaod".len());
        assert!(!blob_aware_eq(&left, &other).unwrap());
        assert!(!blob_aware_eq(&enum_of(left.clone()), &enum_of(other.clone())).unwrap());
        assert!(!blob_aware_eq(&record_of(left.clone()), &record_of(other.clone())).unwrap());
        assert!(!blob_aware_eq(&stream_of(left), &stream_of(other)).unwrap());
    }

    #[test]
    fn blob_equality_recurses_into_compound_values() {
        let blob = annotated_ovb2(
            &orna_value_v1::Blob::from_bytes_with_annotation(
                b"payload".to_vec(),
                "application/octet-stream",
                None,
            )
            .unwrap(),
        );
        let record = |key: &str, value: Value| {
            Value::Record(std::collections::BTreeMap::from([(key.to_owned(), value)]))
        };

        assert!(blob_aware_eq(&record("a", blob.clone()), &record("a", blob.clone())).unwrap());
        assert!(!blob_aware_eq(&record("a", blob.clone()), &record("b", blob.clone())).unwrap());
        assert!(
            blob_aware_eq(
                &Value::Range {
                    lower: Some(Box::new(Value::Int(1.into()))),
                    upper: None,
                    upper_inclusive: false,
                },
                &Value::Range {
                    lower: Some(Box::new(Value::Int(1.into()))),
                    upper: None,
                    upper_inclusive: false,
                },
            )
            .unwrap()
        );
        assert!(
            !blob_aware_eq(
                &Value::Range {
                    lower: Some(Box::new(Value::Int(1.into()))),
                    upper: None,
                    upper_inclusive: false,
                },
                &Value::Range {
                    lower: Some(Box::new(Value::Int(1.into()))),
                    upper: None,
                    upper_inclusive: true,
                },
            )
            .unwrap()
        );
    }

    fn rov3_context(payload: &[u8]) -> Format3Context {
        let database = [1; 16];
        let resolver: std::sync::Arc<dyn orna_value_v1::BlobResolver> =
            std::sync::Arc::new(PayloadResolver {
                payload: payload.to_vec(),
                reads: std::sync::atomic::AtomicUsize::new(0),
            });
        Format3Context::from_persisted(
            database,
            orna_value_v1::Snapshot::cwd(database, [2; 16], 0.into()).unwrap(),
            orna_value_v1::GitHash::Sha1,
            orna_value_v1::NativeOid::from_bytes(&[9; 20]).unwrap(),
            orna_value_v1::NativeOid::from_bytes(&[8; 20]).unwrap(),
            orna_value_v1::AvailabilityState::Available,
            orna_value_v1::OwnerLifetime::new([3; 16]),
            orna_value_v1::Format3Quota::new(8 * 1024 * 1024, 1_u64 << 62).unwrap(),
            resolver,
        )
        .unwrap()
    }

    pub(super) fn rov3_blob_value(payload: &[u8]) -> Value {
        let context = rov3_context(payload);
        let reference = context
            .reference(
                orna_value_v1::ContentIdentity::from_bytes(payload),
                orna_value_v1::NativeOid::from_bytes(&[7; 20]).unwrap(),
            )
            .unwrap();
        let blob = orna_value_v1::Blob::from_reference_with_annotation(
            reference,
            "application/octet-stream",
            None,
        )
        .unwrap();
        Value::ContextualBlob(ContextValue::from_blob(&blob, ValueFormat::Rov3).unwrap())
    }

    /// The payload-free summary a format-3 Blob presents at the REPL boundary.
    fn blob_summary_raw(payload: &[u8]) -> Raw {
        let identity = orna_value_v1::ContentIdentity::from_bytes(payload);
        Raw::Map(vec![
            (Raw::Text("type".to_owned()), Raw::Text("Blob".to_owned())),
            (Raw::Text("length".to_owned()), Raw::Int(identity.length().into())),
            (Raw::Text("sha256".to_owned()), Raw::Bytes(identity.sha256().to_vec())),
            (Raw::Text("suffix".to_owned()), Raw::Null),
            (
                Raw::Text("media_type".to_owned()),
                Raw::Text("application/octet-stream".to_owned()),
            ),
        ])
    }

    fn canonical_summary(value: Value) -> CanonicalValue {
        value
            .canonical_repl_result()
            .unwrap_or_else(|error| panic!("REPL result: {}", error.code()))
    }

    #[test]
    fn repl_result_reports_rov3_blob_as_payload_free_summary() {
        let payload: &[u8] = b"payload";
        let source = rov3_blob_value(payload);
        let Value::ContextualBlob(source_reference) = &source else {
            panic!("a format-3 Blob value");
        };
        // The source reference carries the descriptor OID, so the absence
        // check below is not vacuous.
        assert!(
            source_reference
                .encode()
                .unwrap()
                .windows(20)
                .any(|window| window == [7u8; 20])
        );
        let canonical = canonical_summary(source);
        assert_eq!(
            canonical,
            CanonicalValue::new(blob_summary_raw(payload)).unwrap()
        );
        let encoded = canonical.encode().unwrap();
        assert!(!encoded.windows(payload.len()).any(|window| window == payload));
        assert!(!encoded.windows(20).any(|window| window == [7u8; 20]));
    }

    #[test]
    fn repl_result_summarises_nested_rov3_blobs_in_canonical_order() {
        let payload: &[u8] = b"payload";
        let summary = blob_summary_raw(payload);
        // A tuple or list carries the blob as an ordinary element.
        assert_eq!(
            canonical_summary(Value::Tuple(vec![
                Value::Int(1.into()),
                rov3_blob_value(payload),
            ])),
            CanonicalValue::new(Raw::Tag(
                60015,
                Box::new(Raw::Array(vec![Raw::Int(1.into()), summary.clone()])),
            ))
            .unwrap()
        );
        assert_eq!(
            canonical_summary(Value::List(vec![rov3_blob_value(payload)].into())),
            CanonicalValue::new(Raw::Array(vec![summary.clone()])).unwrap()
        );
        // Record fields keep canonical key order, and the summarised result
        // must still validate as a canonical map.
        let record = Value::Record(std::collections::BTreeMap::from([
            ("b".to_owned(), rov3_blob_value(payload)),
            ("a".to_owned(), Value::Int(1.into())),
        ]));
        assert_eq!(
            canonical_summary(record),
            CanonicalValue::new(Raw::Map(vec![
                (Raw::Text("a".to_owned()), Raw::Int(1.into())),
                (Raw::Text("b".to_owned()), summary),
            ]))
            .unwrap()
        );
    }

    /// The REPL summary is applied only at the REPL boundary. The global
    /// canonical boundary still refuses a format-3 Blob reference.
    #[test]
    fn global_canonical_still_rejects_rov3_blob() {
        match rov3_blob_value(b"payload").canonical() {
            Err(error) => assert_eq!(error.code(), "ORNA-EVAL-VALUE"),
            Ok(_) => panic!("Value::canonical must keep refusing a format-3 Blob"),
        }
    }

    /// A row-store overflow reference (tag 60113) has no Value form. It is not
    /// summarised at the REPL boundary; if one reaches the evaluator it is
    /// refused, so no reference or payload crosses the boundary.
    #[test]
    fn overflow_reference_is_refused_by_the_evaluator() {
        let format3 = rov3_context(b"payload");
        let overflow = ContextValue::new_in_context(
            &format3,
            ValueFormat::Rov3,
            Raw::Tag(
                orna_value_v1::ROV3_OVERFLOW_TAG,
                Box::new(Raw::Array(vec![Raw::Bytes(vec![7; 20])])),
            ),
        )
        .unwrap();
        let functions = Functions::new();
        let mut context = Context {
            limits: Limits::default(),
            steps: 0,
            calls: 0,
            functions: &functions,
            aliases: None,
            session_functions: None,
            repl_bindings: true,
            restrict_function_names: true,
            reject_unhandled_field_calls: true,
            effects: None,
            namespace: None,
            transfer: None,
            materializations: Vec::new(),
            materialization_clock_unavailable: false,
            reuse: CallReuse::default(),
            sys_host: None,
            cancellation: None,
        };
        match Value::from_context_value(&overflow, &mut context, 0) {
            Err(error) => assert_eq!(error.code(), "ORNA-EVAL-UNSUPPORTED"),
            Ok(_) => panic!("an overflow reference must not become a Value"),
        }
    }
}

#[cfg(test)]
mod standard_layer_merge_tests {
    use super::{Functions, StandardLayer, StandardLayerConflict};
    use std::{collections::BTreeMap, sync::Arc};

    fn layer(alias: &str) -> Arc<StandardLayer> {
        Arc::new(StandardLayer::new(
            BTreeMap::new(),
            BTreeMap::from([(alias.to_owned(), alias.to_owned())]),
        ))
    }

    #[test]
    fn second_distinct_standard_layer_is_refused_without_mutating_the_table() {
        let first = layer("first");
        let mut target = Functions {
            standard: Some(Arc::clone(&first)),
            ..Functions::default()
        };
        let other = Functions {
            standard: Some(layer("second")),
            local_aliases: BTreeMap::from([("extra".to_owned(), "extra".to_owned())]),
            ..Functions::default()
        };
        assert_eq!(target.extend(other), Err(StandardLayerConflict));
        assert!(Arc::ptr_eq(target.standard.as_ref().unwrap(), &first));
        assert!(!target.local_aliases.contains_key("extra"));
    }

    #[test]
    fn the_same_shared_standard_layer_merges() {
        let shared = layer("shared");
        let mut target = Functions {
            standard: Some(Arc::clone(&shared)),
            ..Functions::default()
        };
        let other = Functions {
            standard: Some(Arc::clone(&shared)),
            local_aliases: BTreeMap::from([("extra".to_owned(), "extra".to_owned())]),
            ..Functions::default()
        };
        assert_eq!(target.extend(other), Ok(()));
        assert!(target.local_aliases.contains_key("extra"));
    }
}

/// Proof that folding a row into the materialization digest allocates a
/// bounded amount per row. A thread-local counter only tallies allocations made
/// by the test thread while `COUNTING` is set, so parallel tests do not skew it.
#[cfg(test)]
mod output_digest_allocation_proof {
    use super::*;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    use std::sync::atomic::{AtomicUsize, Ordering};

    thread_local! {
        static COUNTING: Cell<bool> = const { Cell::new(false) };
    }
    static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

    struct CountingAllocator;

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if COUNTING.with(Cell::get) {
                ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            }
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static GLOBAL: CountingAllocator = CountingAllocator;

    fn sample_row() -> Value {
        Value::Record(BTreeMap::from([
            ("object".to_owned(), Value::Blob(vec![7; 32])),
            (
                "name".to_owned(),
                Value::String("materialized output row".repeat(4)),
            ),
            (
                "items".to_owned(),
                Value::List(List::from(
                    (0..16)
                        .map(|number| Value::Int(BigInt::from(number)))
                        .collect::<Vec<_>>(),
                )),
            ),
            ("valid".to_owned(), Value::Bool(true)),
        ]))
    }

    /// Runs `work` with allocation counting enabled on this thread and returns
    /// the number of heap allocations it performed.
    fn count_allocations(work: impl FnOnce()) -> usize {
        let before = ALLOCATIONS.load(Ordering::Relaxed);
        COUNTING.with(|counting| counting.set(true));
        work();
        COUNTING.with(|counting| counting.set(false));
        ALLOCATIONS.load(Ordering::Relaxed) - before
    }

    #[test]
    fn output_digest_row_allocations_are_counted() {
        const ROWS: usize = 256;
        let row = sample_row();
        let expected_len = row
            .canonical_bytes()
            .expect("sample row is canonical")
            .len();

        // Before: the per-row fold cloned the value before canonicalising it.
        let legacy = count_allocations(|| {
            for _ in 0..ROWS {
                let encoded = row
                    .clone()
                    .canonical()
                    .and_then(|canonical| canonical.encode().map_err(|_| error("ORNA-EVAL-VALUE")))
                    .expect("legacy row encodes");
                std::hint::black_box(encoded);
            }
        });

        // After: the digest fold borrows the row.
        let mut digest = OutputDigest::new();
        let borrowing = count_allocations(|| {
            for _ in 0..ROWS {
                digest.push(&row);
            }
        });
        eprintln!(
            "OUTPUT_DIGEST_ALLOCATIONS rows={ROWS} legacy_total={legacy} legacy_per_row={} borrowing_total={borrowing} borrowing_per_row={}",
            legacy / ROWS,
            borrowing / ROWS
        );
        assert!(digest.valid);
        assert_eq!(digest.bytes, expected_len * ROWS);
        assert!(borrowing < legacy);
    }
}

#[cfg(test)]
mod output_digest_equivalence {
    use super::blob_equality_tests::rov3_blob_value;
    use super::*;

    /// One value per `Value` variant (31 arms), so the borrowed digest encoding
    /// is checked against the legacy clone path on every arm.
    fn every_variant() -> Vec<Value> {
        let closure = Closure {
            parameters: Vec::new(),
            body: Expr::Name {
                text: "x".to_owned(),
                span: orna_syntax_v1::SyntaxSpan::new(0, 0),
            },
            captured: Scope(
                BTreeMap::new(),
                BTreeSet::new(),
                BTreeSet::new(),
                NominalDefinitions::new(),
                BTreeSet::new(),
            ),
            namespace: None,
        };
        let relation = RelationPlan {
            source: "rows".to_owned(),
            source_identity: RelationReadScope(0),
            shared_filter_group: None,
            source_union: None,
            source_values: None,
            stages: Vec::new(),
        };
        vec![
            Value::Null,
            Value::Unit,
            Value::Bool(true),
            Value::Int(BigInt::from(-7)),
            Value::Decimal(DecimalValue::new(BigInt::from(125), BigInt::from(-2)).unwrap()),
            Value::Money {
                amount: DecimalValue::new(BigInt::from(1), BigInt::from(0)).unwrap(),
                currency: [0x55; 16],
            },
            Value::Float(1.5_f64.to_bits()),
            Value::String("text".to_owned()),
            Value::Blob(vec![1, 2, 3]),
            Value::AnnotatedBlob(Raw::Text("annotated".to_owned())),
            rov3_blob_value(b"payload"),
            Value::Date("2026-10-26".to_owned()),
            Value::Uuid([3; 16]),
            Value::Digest([4; 32]),
            Value::Reference(CanonicalValue::new(Raw::Int(3.into())).expect("canonical")),
            Value::TableReference(CanonicalValue::new(Raw::Int(4.into())).expect("canonical")),
            Value::Instant {
                unix_seconds: 1,
                nanosecond: 0,
            },
            Value::Duration {
                seconds: BigInt::from(1),
                nanosecond: 5,
            },
            Value::Period {
                days: BigInt::from(2),
            },
            Value::Error(error("ORNA-TEST-ERROR")),
            Value::Range {
                lower: Some(Box::new(Value::Int(BigInt::from(1)))),
                upper: None,
                upper_inclusive: false,
            },
            Value::List(List::from(vec![Value::Bool(false)])),
            Value::Stream {
                values: vec![Value::Bool(false)],
                source_label: "source".to_owned(),
                source_digest: [0; 32],
                position: 0,
                provider: None,
            },
            Value::Relation(relation),
            Value::Tuple(vec![Value::Null]),
            Value::Record(BTreeMap::from([("field".to_owned(), Value::Bool(true))])),
            Value::NominalRecord {
                type_id: Raw::Int(1.into()),
                fields: vec![(Raw::Text("field".to_owned()), Value::Null)],
            },
            Value::Enum {
                type_id: Raw::Int(1.into()),
                variant_id: Raw::Int(2.into()),
                payload: Some(Box::new(Value::Null)),
            },
            Value::Option(Some(Box::new(Value::Int(BigInt::from(5))))),
            Value::Function {
                name: "f".to_owned(),
                nominal_definitions: NominalDefinitions::new(),
            },
            Value::Closure(Arc::new(closure)),
        ]
    }

    fn outcome(result: Result<Vec<u8>, EvaluationError>) -> Result<Vec<u8>, String> {
        result.map_err(|failure| failure.code().to_owned())
    }

    /// Spec: Orna-1.1.0.md:7400-7417. The digest fold must encode each row to
    /// exactly the bytes, or exactly the rejection code, of the legacy path.
    #[test]
    fn borrowed_canonical_bytes_match_legacy_clone_path_for_every_variant() {
        let values = every_variant();
        assert_eq!(values.len(), 31);
        let mut accepted = 0;
        let mut rejected = 0;
        for value in values {
            let borrowed = outcome(value.canonical_bytes());
            let legacy = outcome(
                value
                    .clone()
                    .canonical()
                    .and_then(|canonical| canonical.encode().map_err(|_| error("ORNA-EVAL-VALUE"))),
            );
            assert_eq!(borrowed, legacy, "{value:?}");
            if borrowed.is_ok() {
                accepted += 1;
            } else {
                rejected += 1;
            }
        }
        assert!(accepted > 0 && rejected > 0);
    }
}
