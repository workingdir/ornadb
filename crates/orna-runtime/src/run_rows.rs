//! The `sys.Run` rows of the HIR host's `sys.rt.runs` relation (hir-host.md A5, slice HA8).
//!
//! [`sys_run_rows`] encodes each checked [`SysRunProjection`] as one canonical record whose fields are the fields of
//! `sys.Run` in the frozen contract (`crates/orna-sys/src/system_api_inventory.json`), in that order. Every reference
//! is the canonical form of the checked row reference the projection already holds, so the executor reads
//! `function` back as a typed `sys.FunctionRef`. `sys.ConsumerIdentity` is the format-3 digest of the consumer's four
//! natural components, the same one a migration records for the checkpoint.
//!
//! The encoding is all or nothing. A relation whose rows cannot all be encoded is not served in part: the caller
//! registers it unsupported, because a missing row would make `sys.rt.runs.count()` a wrong answer.

use std::time::{SystemTime, UNIX_EPOCH};

use num_bigint::BigInt;
use orna_value::{
    ENUM_TAG, INSTANT_TAG, OPAQUE_SYSTEM_ID_TAG, OPTION_TAG, Raw, RowRef, UUID_TAG, enum_type_id,
    enum_variant_id,
};

use crate::observation::{RunObservationStatus, SysRunProjection};
use crate::stream_carry::consumer_identity_digest;

/// The `sys.Run` row of every run in `runs`, in order, or `None` when any run cannot be encoded honestly.
///
/// A run that records a failure cannot: `sys.Run.failure` is a `sys.Diagnostic`, and the mapping from a retained
/// diagnostic to that row is not written (`ornadb-5il1.280`), so no row can stand in for it.
pub(crate) fn sys_run_rows(runs: &[SysRunProjection]) -> Option<Vec<Raw>> {
    runs.iter().map(run_row).collect()
}

fn run_row(run: &SysRunProjection) -> Option<Raw> {
    if run.failure.is_some() {
        return None;
    }
    let consumer = &run.consumer_identity;
    let consumer_digest = consumer_identity_digest(
        consumer.principal.as_str(),
        consumer.root.as_str(),
        consumer.function.as_str(),
        consumer.binding.as_str(),
    )
    .ok()?;
    let ended = match run.ended {
        Some(time) => Some(instant(time)?),
        None => None,
    };
    Some(record(vec![
        ("reference", reference(run.reference.as_row_ref())?),
        ("id", opaque_id("sys.RunId", uuid(run.id))),
        (
            "consumer_identity",
            opaque_id("sys.ConsumerIdentity", Raw::Bytes(consumer_digest.to_vec())),
        ),
        ("function", reference(run.function.as_row_ref())?),
        (
            "source_identity",
            option(run.source_identity.clone().map(Raw::Text)),
        ),
        ("snapshot", reference(run.snapshot.as_row_ref())?),
        ("started", instant(run.started)?),
        ("ended", option(ended)),
        ("status_at_snapshot", status(run.status_at_snapshot)),
        ("observed_at", instant(run.observed_at)?),
        (
            "runtime_id",
            option(
                run.runtime_id
                    .map(|id| opaque_id("sys.RuntimeId", uuid(id))),
            ),
        ),
        ("live", Raw::Bool(run.live)),
        ("invocation", reference(run.invocation.as_row_ref())?),
        (
            "checkpoint_count",
            Raw::Int(BigInt::from(run.checkpoint_count)),
        ),
        ("failure", option(None)),
    ]))
}

fn record(fields: Vec<(&str, Raw)>) -> Raw {
    Raw::Map(
        fields
            .into_iter()
            .map(|(name, value)| (Raw::Text(name.to_owned()), value))
            .collect(),
    )
}

/// `None` is the canonical option none, `Some(value)` the option some.
fn option(value: Option<Raw>) -> Raw {
    let body = match value {
        None => vec![Raw::Int(BigInt::from(0))],
        Some(value) => vec![Raw::Int(BigInt::from(1)), value],
    };
    Raw::Tag(OPTION_TAG, Box::new(Raw::Array(body)))
}

fn uuid(bytes: [u8; 16]) -> Raw {
    Raw::Tag(UUID_TAG, Box::new(Raw::Bytes(bytes.to_vec())))
}

/// A nominal `sys` identifier: its closed name and its representation.
fn opaque_id(name: &str, representation: Raw) -> Raw {
    Raw::Tag(
        OPAQUE_SYSTEM_ID_TAG,
        Box::new(Raw::Array(vec![Raw::Text(name.to_owned()), representation])),
    )
}

/// The canonical form of a checked row reference, which the executor reads back as the reference.
fn reference(row: &RowRef) -> Option<Raw> {
    row.to_canonical().ok().map(|value| value.raw().clone())
}

/// An `Instant` as its whole Unix seconds and the nanoseconds past them, which are below one second.
fn instant(time: SystemTime) -> Option<Raw> {
    let (seconds, nanosecond) = match time.duration_since(UNIX_EPOCH) {
        Ok(after) => (i64::try_from(after.as_secs()).ok()?, after.subsec_nanos()),
        Err(before) => {
            let before = before.duration();
            let whole = i64::try_from(before.as_secs()).ok()?;
            match before.subsec_nanos() {
                0 => (-whole, 0),
                nanos => (-whole - 1, 1_000_000_000 - nanos),
            }
        }
    };
    Some(Raw::Tag(
        INSTANT_TAG,
        Box::new(Raw::Array(vec![
            Raw::Int(BigInt::from(seconds)),
            Raw::Int(BigInt::from(nanosecond)),
        ])),
    ))
}

/// A payload-free `sys.RunStatus` variant in its canonical enum form.
fn status(status: RunObservationStatus) -> Raw {
    let variant = match status {
        RunObservationStatus::Starting => "starting",
        RunObservationStatus::Running => "running",
        RunObservationStatus::Completed => "completed",
        RunObservationStatus::Failed => "failed",
        RunObservationStatus::Cancelled => "cancelled",
        RunObservationStatus::Orphaned => "orphaned",
    };
    Raw::Tag(
        ENUM_TAG,
        Box::new(Raw::Array(vec![
            uuid(enum_type_id("sys.RunStatus")),
            uuid(enum_variant_id("sys.RunStatus", variant)),
            Raw::Null,
        ])),
    )
}
