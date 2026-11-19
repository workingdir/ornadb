use crate::*;

/// Equality for `==` and `!=`: Blob values are compared by their annotation and
/// exact content, not by OVB-2 or format-3 representation. Containers and
/// options are compared element-wise so that a Blob nested inside them is
/// decided the same way. Every other pair keeps ordinary structural equality.
pub(crate) fn blob_aware_eq(left: &Value, right: &Value) -> Result<bool, EvaluationError> {
    match (left, right) {
        (Value::List(left), Value::List(right)) => {
            blob_aware_seq_eq(left.as_slice(), right.as_slice())
        }
        (Value::Tuple(left), Value::Tuple(right)) => blob_aware_seq_eq(left, right),
        (Value::Option(left), Value::Option(right)) => blob_aware_option_eq(left, right),
        // An Error's identity is its canonical code, message, causes and
        // details. The raised-at source span and diagnostic are provenance,
        // so equal errors raised at different sites still compare equal.
        (Value::Error(left), Value::Error(right)) => {
            match (left.canonical_error(), right.canonical_error()) {
                (Some(left), Some(right)) => Ok(left == right),
                _ => Ok(left == right),
            }
        }
        (Value::Record(left), Value::Record(right)) => blob_aware_record_eq(left, right),
        (
            Value::NominalRecord {
                type_id: left_type,
                fields: left_fields,
            },
            Value::NominalRecord {
                type_id: right_type,
                fields: right_fields,
            },
        ) => blob_aware_nominal_record_eq(left_type, left_fields, right_type, right_fields),
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
        ) => blob_aware_enum_eq(
            left_type,
            left_variant,
            left_payload.as_deref(),
            right_type,
            right_variant,
            right_payload.as_deref(),
        ),
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
        ) => blob_aware_range_eq(
            left_lower.as_deref(),
            left_upper.as_deref(),
            *left_inclusive,
            right_lower.as_deref(),
            right_upper.as_deref(),
            *right_inclusive,
        ),
        (Value::Stream { .. }, Value::Stream { .. }) => blob_aware_stream_eq(left, right),
        (
            Value::AnnotatedBlob(_) | Value::ContextualBlob(_),
            Value::AnnotatedBlob(_) | Value::ContextualBlob(_),
        ) => blob_aware_annotated_eq(left, right),
        _ => Ok(left == right),
    }
}

fn blob_aware_option_eq(
    left: &Option<Box<Value>>,
    right: &Option<Box<Value>>,
) -> Result<bool, EvaluationError> {
    match (left.as_deref(), right.as_deref()) {
        (None, None) => Ok(true),
        (Some(left), Some(right)) => blob_aware_eq(left, right),
        _ => Ok(false),
    }
}

fn blob_aware_record_eq(
    left: &BTreeMap<String, Value>,
    right: &BTreeMap<String, Value>,
) -> Result<bool, EvaluationError> {
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

fn blob_aware_nominal_record_eq(
    left_type: &Raw,
    left_fields: &[(Raw, Value)],
    right_type: &Raw,
    right_fields: &[(Raw, Value)],
) -> Result<bool, EvaluationError> {
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

fn blob_aware_enum_eq(
    left_type: &Raw,
    left_variant: &Raw,
    left_payload: Option<&Value>,
    right_type: &Raw,
    right_variant: &Raw,
    right_payload: Option<&Value>,
) -> Result<bool, EvaluationError> {
    if left_type != right_type || left_variant != right_variant {
        return Ok(false);
    }
    match (left_payload, right_payload) {
        (None, None) => Ok(true),
        (Some(left), Some(right)) => blob_aware_eq(left, right),
        _ => Ok(false),
    }
}

fn blob_aware_range_eq(
    left_lower: Option<&Value>,
    left_upper: Option<&Value>,
    left_inclusive: bool,
    right_lower: Option<&Value>,
    right_upper: Option<&Value>,
    right_inclusive: bool,
) -> Result<bool, EvaluationError> {
    if left_inclusive != right_inclusive {
        return Ok(false);
    }
    Ok(optional_blob_aware_eq(left_lower, right_lower)?
        && optional_blob_aware_eq(left_upper, right_upper)?)
}

/// Compares two streams by their replay metadata and then their items.
fn blob_aware_stream_eq(left: &Value, right: &Value) -> Result<bool, EvaluationError> {
    let (
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
    ) = (left, right)
    else {
        return Ok(false);
    };
    if left_label != right_label
        || left_digest != right_digest
        || left_position != right_position
        || left_provider != right_provider
    {
        return Ok(false);
    }
    blob_aware_seq_eq(left_values, right_values)
}

fn blob_aware_annotated_eq(left: &Value, right: &Value) -> Result<bool, EvaluationError> {
    let left = annotated_blob(left)?;
    let right = annotated_blob(right)?;
    left.value_eq(&right).map_err(blob_equality_error)
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
fn annotated_blob(value: &Value) -> Result<orna_value::Blob, EvaluationError> {
    let context_value = match value {
        Value::AnnotatedBlob(raw) => ContextValue::new(ValueFormat::Ovb2, raw.clone()),
        Value::ContextualBlob(value) => Ok(value.clone()),
        _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
    }
    .map_err(blob_equality_error)?;
    context_value.blob().map_err(blob_equality_error)
}

/// Maps Blob comparison failures to the portable codes the Blob contracts name.
#[cfg(feature = "system-host-bindings")]
fn blob_equality_error(failure: ValueError) -> EvaluationError {
    match failure {
        ValueError::ContentUnavailable => sys_error(SysFailureCode::BlobUnavailable),
        ValueError::ContentDigestMismatch => sys_error(SysFailureCode::BlobCorrupt),
        ValueError::InvalidContext => sys_error(SysFailureCode::BlobReferenceContext),
        _ => error("ORNA-EVAL-VALUE"),
    }
}

/// Without host bindings the closed `sys` vocabulary is not linked; the portable
/// code bytes stay identical.
#[cfg(not(feature = "system-host-bindings"))]
fn blob_equality_error(failure: ValueError) -> EvaluationError {
    match failure {
        ValueError::ContentUnavailable => error(non_host_names::BLOB_UNAVAILABLE),
        ValueError::ContentDigestMismatch => error(non_host_names::BLOB_CORRUPT),
        ValueError::InvalidContext => error(non_host_names::BLOB_REFERENCE_CONTEXT),
        _ => error("ORNA-EVAL-VALUE"),
    }
}

/// The annotation and content identity of one inline OVB-2 Blob. Two inline
/// Blobs that `==` relates always share this pair, so a pair whose pairs differ
/// is unequal and needs no comparison of content bytes.
type BlobFingerprint = (MediaAnnotation, ContentIdentity);

/// Equality key for collection and relation distinct folds. Values that `==`
/// relates always share a key, so the key only narrows the candidates that
/// `DistinctValueFold::insert` decides with `==`.
///
/// `skeleton` is the canonical form of the value with every Blob node replaced
/// by `null`; the canonical encoder rejects Blob tags, so Blob payloads are not
/// encoded. `blob_positions` records each replaced Blob's pre-order index in the
/// skeleton, so Blobs at different positions never become candidates for one
/// another.
#[derive(Clone, PartialEq, Eq, Hash)]
struct DistinctKey {
    skeleton: Vec<u8>,
    blob_positions: Vec<usize>,
}

/// The distinct identity of one value. `fingerprints` lists the fingerprint of
/// each Blob in `key.blob_positions` order; it is `None` when any Blob is not an
/// inline OVB-2 Blob with a readable fingerprint, so `==` must decide that value
/// against every candidate of its key.
struct DistinctIdentity {
    key: DistinctKey,
    fingerprints: Option<Vec<BlobFingerprint>>,
}

/// Encodes the distinct identity of `value`. Floats are unsupported in distinct folds.
fn distinct_identity(value: &Value) -> Result<DistinctIdentity, EvaluationError> {
    if value.contains_float() {
        return Err(error("ORNA-EVAL-UNSUPPORTED"));
    }
    // Blobs are replaced by placeholders before `raw()` runs, so the content
    // bytes of inline Blobs are never copied into the skeleton.
    let mut blobs = Vec::new();
    let stripped = strip_blobs(value, &mut blobs);
    let mut slots = Vec::new();
    let mut next_index = 0;
    let skeleton = identity_skeleton(stripped.raw()?, &mut next_index, &mut slots);
    let skeleton = CanonicalValue::new(skeleton)
        .map_err(|_| error("ORNA-EVAL-VALUE"))?
        .encode()
        .map_err(|_| error("ORNA-EVAL-VALUE"))?;
    let blob_positions = slots.iter().map(|&(position, _)| position).collect();
    let fingerprints = slots
        .iter()
        .map(|&(_, slot)| {
            slot.and_then(|slot| blobs.get(slot))
                .and_then(|raw| ovb2_blob_fingerprint(raw).ok())
        })
        .collect::<Option<Vec<_>>>();
    Ok(DistinctIdentity {
        key: DistinctKey {
            skeleton,
            blob_positions,
        },
        fingerprints,
    })
}

/// Tag of the placeholder `strip_blobs` writes for each inline OVB-2 Blob; its
/// integer payload is the Blob's index in `blobs`. No admitted raw carries this
/// number: `validate_tag` and `validate_context_tag` accept only their own tag
/// numbers, and `u64::MAX` is in neither table. A genuine Blob node therefore
/// never reads as a placeholder slot in `identity_skeleton`.
pub(crate) const DISTINCT_SLOT_TAG: u64 = u64::MAX;

/// Returns `value` with each inline OVB-2 Blob replaced by a placeholder whose
/// integer payload is the Blob's index in `blobs`. Every other node is copied
/// as it is; a Blob in another form keeps its own raw node and so has no slot.
fn strip_blobs<'a>(value: &'a Value, blobs: &mut Vec<&'a Raw>) -> Value {
    match value {
        Value::AnnotatedBlob(raw) => {
            let slot = blobs.len();
            blobs.push(raw);
            Value::AnnotatedBlob(Raw::Tag(DISTINCT_SLOT_TAG, Box::new(Raw::Int(slot.into()))))
        }
        Value::List(values) => Value::List(List::from(
            values
                .as_slice()
                .iter()
                .map(|value| strip_blobs(value, blobs))
                .collect::<Vec<_>>(),
        )),
        Value::Tuple(values) => Value::Tuple(
            values
                .iter()
                .map(|value| strip_blobs(value, blobs))
                .collect(),
        ),
        Value::Option(payload) => Value::Option(
            payload
                .as_deref()
                .map(|value| Box::new(strip_blobs(value, blobs))),
        ),
        Value::Record(fields) => Value::Record(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), strip_blobs(value, blobs)))
                .collect(),
        ),
        Value::NominalRecord { type_id, fields } => Value::NominalRecord {
            type_id: type_id.clone(),
            fields: fields
                .iter()
                .map(|(key, value)| (key.clone(), strip_blobs(value, blobs)))
                .collect(),
        },
        Value::Enum {
            type_id,
            variant_id,
            payload,
        } => Value::Enum {
            type_id: type_id.clone(),
            variant_id: variant_id.clone(),
            payload: payload
                .as_deref()
                .map(|value| Box::new(strip_blobs(value, blobs))),
        },
        other => other.clone(),
    }
}

/// Replaces every Blob node with `null`. Each Blob node records its pre-order
/// index and the placeholder slot it carries, `None` when it is not a placeholder.
pub(crate) fn identity_skeleton(
    raw: Raw,
    next_index: &mut usize,
    slots: &mut Vec<(usize, Option<usize>)>,
) -> Raw {
    let index = *next_index;
    *next_index += 1;
    match raw {
        Raw::Tag(DISTINCT_SLOT_TAG, payload) => {
            let slot = match *payload {
                Raw::Int(slot) => slot.to_usize(),
                _ => None,
            };
            slots.push((index, slot));
            Raw::Null
        }
        Raw::Tag(OVB2_BLOB_TAG | ROV3_BLOB_TAG | SOV3_BLOB_TAG, _) => {
            // A Blob node that was not stripped has no placeholder slot, so its
            // fingerprint is unavailable and its key stays undecided.
            slots.push((index, None));
            Raw::Null
        }
        Raw::Array(items) => Raw::Array(
            items
                .into_iter()
                .map(|item| identity_skeleton(item, next_index, slots))
                .collect(),
        ),
        Raw::Map(entries) => Raw::Map(
            entries
                .into_iter()
                .map(|(key, value)| {
                    let key = identity_skeleton(key, next_index, slots);
                    let value = identity_skeleton(value, next_index, slots);
                    (key, value)
                })
                .collect(),
        ),
        Raw::Tag(tag, payload) => Raw::Tag(
            tag,
            Box::new(identity_skeleton(*payload, next_index, slots)),
        ),
        other => other,
    }
}

/// The kept values of one distinct key. `all` lists them in insertion order;
/// a kept value with fingerprints sits in `by_fingerprint`, and any other kept
/// value sits in `undecided`. Both lists are in insertion order.
#[derive(Clone, Default)]
struct DistinctBucket {
    all: Vec<usize>,
    by_fingerprint: HashMap<Vec<BlobFingerprint>, Vec<usize>>,
    undecided: Vec<usize>,
}

/// Keeps the first original value of each equality class under `==`. Values
/// that share a `DistinctKey` are decided with `blob_aware_eq`, the relation
/// `==` uses, so a value is kept exactly when `==` finds it unequal to every
/// kept value. Content that `==` cannot decide without the bytes fails with the
/// same error (ORNA-BLOB-005). Each query invocation constructs its own fold so
/// paired recursive queries cannot suppress one another's rows.
#[derive(Clone, Default)]
pub(crate) struct DistinctValueFold {
    buckets: HashMap<DistinctKey, DistinctBucket>,
    pub(crate) values: Vec<Value>,
}

impl DistinctValueFold {
    pub(crate) fn insert(&mut self, value: Value) -> Result<bool, EvaluationError> {
        let DistinctIdentity { key, fingerprints } = distinct_identity(&value)?;
        let has_blobs = !key.blob_positions.is_empty();
        let bucket = self.buckets.entry(key).or_default();
        if !has_blobs {
            if !bucket.all.is_empty() {
                return Ok(false);
            }
        } else if blob_candidate_equal(&self.values, bucket, fingerprints.as_deref(), &value)? {
            return Ok(false);
        }
        let index = self.values.len();
        bucket.all.push(index);
        match fingerprints {
            Some(fingerprints) => bucket
                .by_fingerprint
                .entry(fingerprints)
                .or_default()
                .push(index),
            None => bucket.undecided.push(index),
        }
        self.values.push(value);
        Ok(true)
    }

    pub(crate) fn len(&self) -> usize {
        self.values.len()
    }

    pub(crate) fn into_values(self) -> Vec<Value> {
        self.values
    }
}

/// Whether `==` relates `value` to a kept value of `bucket`. Without
/// fingerprints every kept value is a candidate. With them, the candidates are
/// the kept values with the same fingerprints and the undecided ones, merged in
/// insertion order: a kept inline Blob with other fingerprints is unequal to
/// `value` and cannot change the result or its first error.
fn blob_candidate_equal(
    values: &[Value],
    bucket: &DistinctBucket,
    fingerprints: Option<&[BlobFingerprint]>,
    value: &Value,
) -> Result<bool, EvaluationError> {
    let Some(fingerprints) = fingerprints else {
        for &index in &bucket.all {
            if blob_aware_eq(&values[index], value)? {
                return Ok(true);
            }
        }
        return Ok(false);
    };
    let same: &[usize] = bucket
        .by_fingerprint
        .get(fingerprints)
        .map_or(&[], Vec::as_slice);
    let undecided = bucket.undecided.as_slice();
    let (mut same_at, mut undecided_at) = (0, 0);
    loop {
        let index = match (same.get(same_at), undecided.get(undecided_at)) {
            (Some(&same_index), Some(&undecided_index)) if same_index < undecided_index => {
                same_at += 1;
                same_index
            }
            (Some(_), Some(&undecided_index)) => {
                undecided_at += 1;
                undecided_index
            }
            (Some(&same_index), None) => {
                same_at += 1;
                same_index
            }
            (None, Some(&undecided_index)) => {
                undecided_at += 1;
                undecided_index
            }
            (None, None) => return Ok(false),
        };
        if blob_aware_eq(&values[index], value)? {
            return Ok(true);
        }
    }
}
