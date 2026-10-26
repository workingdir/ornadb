//! The `sys.RuntimeInfo` record that `sys.rt.info()` returns.
//!
//! Each field is a coordinate the build declares, the canonical codec, the
//! timezone dataset, the Rust standard library, or a fact the host attached to
//! this activation. Nothing is defaulted here; an absent fact is reported by
//! the host before the evaluator is reached.

use orna_foundation_v1::{CanonicalSnapshot, CanonicalValue, GitHash, snapshot_reference};
use orna_sys_v1::{
    BUILD_ID, IMPLEMENTATION_NAME, IMPLEMENTATION_VERSION, LANGUAGE_VERSION,
    PRESENTATION_PROTOCOL_VERSION, SUPPORTED_PROFILES, SYS_VERSION, StdSnapshotPin, SysHostContext,
};
use orna_value_v1::{OVB2_VERSION, Raw};

use crate::{EvaluationError, TIMEZONE_DATASET_VERSION, Value, error};

/// The host context one activation is attached with, together with the
/// `sys.RuntimeInfo` record it determines. The record is computed once, when
/// the host attaches the context, so `sys.rt.info()` only reads it back.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SysHostAttachment {
    context: SysHostContext,
    info: Result<Value, EvaluationError>,
}

impl SysHostAttachment {
    /// Attaches `context`, computing the `sys.RuntimeInfo` record it determines.
    pub fn new(context: SysHostContext) -> Self {
        let info = runtime_info(&context);
        Self { context, info }
    }

    /// The host context this attachment was built from.
    #[must_use]
    pub fn context(&self) -> &SysHostContext {
        &self.context
    }

    /// The precomputed `sys.RuntimeInfo` record, or the failure reported when a
    /// required fact could not be read at attach time.
    pub(crate) fn runtime_info(&self) -> Result<Value, EvaluationError> {
        self.info.clone()
    }
}

/// Builds the `sys.RuntimeInfo` record for one activation
/// (34-system-reference.md, `sys.RuntimeInfo`).
pub(crate) fn runtime_info(context: &SysHostContext) -> Result<Value, EvaluationError> {
    let supported_profiles = SUPPORTED_PROFILES
        .iter()
        .map(|profile| Value::String((*profile).to_owned()))
        .collect();
    let fields = [
        (
            "language_version",
            Value::String(LANGUAGE_VERSION.to_owned()),
        ),
        ("sys_version", Value::String(SYS_VERSION.to_owned())),
        (
            "canonical_orna_codec_version",
            Value::String(OVB2_VERSION.to_owned()),
        ),
        (
            "repository_layout_version",
            Value::String(context.repository_layout_version.clone()),
        ),
        (
            "storage_manifest_version",
            Value::String(context.storage_manifest_version.clone()),
        ),
        (
            "presentation_protocol_version",
            Value::String(PRESENTATION_PROTOCOL_VERSION.to_owned()),
        ),
        (
            "implementation_name",
            Value::String(IMPLEMENTATION_NAME.to_owned()),
        ),
        (
            "implementation_version",
            Value::String(IMPLEMENTATION_VERSION.to_owned()),
        ),
        ("build_id", Value::String(BUILD_ID.to_owned())),
        ("supported_profiles", Value::list(supported_profiles)),
        ("runtime", Value::Uuid(context.runtime_id)),
        (
            "mode",
            Value::Enum {
                type_id: Raw::Text("sys.RuntimeMode".to_owned()),
                variant_id: Raw::Text(context.mode.label().to_owned()),
                payload: None,
            },
        ),
        ("read_only", Value::Bool(context.read_only)),
        ("std_snapshot", std_snapshot_field(context)?),
        ("unicode_version", Value::String(unicode_version())),
        (
            "timezone_database_version",
            Value::Option(Some(Box::new(Value::String(
                TIMEZONE_DATASET_VERSION.to_owned(),
            )))),
        ),
    ];
    Ok(Value::Record(
        fields
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect(),
    ))
}

/// The `std_snapshot` field: the pinned standard commit as a `sys.SnapshotRef`,
/// or `none` when the project pins no standard profile.
///
/// The commit is keyed under the attached database. A pin whose commit id
/// cannot be read back is reported as `sys.snapshot.incomplete`, never guessed.
fn std_snapshot_field(context: &SysHostContext) -> Result<Value, EvaluationError> {
    let oid = match &context.std_snapshot {
        StdSnapshotPin::Unpinned => return Ok(Value::Option(None)),
        StdSnapshotPin::Unreadable => return Err(error("sys.snapshot.incomplete")),
        StdSnapshotPin::Commit(oid) => oid,
    };
    let algorithm = match oid.len() {
        20 => GitHash::Sha1,
        32 => GitHash::Sha256,
        _ => return Err(error("sys.snapshot.incomplete")),
    };
    let snapshot = CanonicalSnapshot::Commit {
        database: context.database_id,
        algorithm,
        oid: oid.clone(),
    };
    let encoded = snapshot_reference(context.database_id, snapshot)
        .map_err(|_| error("sys.snapshot.incomplete"))?
        .into_row_ref()
        .encode()
        .map_err(|_| error("sys.snapshot.incomplete"))?;
    let value = CanonicalValue::decode(&encoded).map_err(|_| error("sys.snapshot.incomplete"))?;
    Ok(Value::Option(Some(Box::new(Value::Reference(value)))))
}

/// The Unicode version of the character tables this build uses, `major.minor.patch`.
fn unicode_version() -> String {
    let (major, minor, patch) = char::UNICODE_VERSION;
    format!("{major}.{minor}.{patch}")
}

#[cfg(test)]
mod tests {
    use super::runtime_info;
    use crate::{EvaluationError, Value};
    use orna_sys_v1::{RuntimeMode, StdSnapshotPin, SysHostContext};
    use std::path::PathBuf;

    fn context_pinned(std_snapshot: StdSnapshotPin) -> SysHostContext {
        SysHostContext {
            database_id: [1; 16],
            repository_id: [2; 16],
            runtime_id: [0x11; 16],
            root: PathBuf::from("/repo"),
            read_only: false,
            mode: RuntimeMode::Run,
            repository_layout_version: "3".to_owned(),
            storage_manifest_version: "store-3".to_owned(),
            std_snapshot,
        }
    }

    /// Extracts the `std_snapshot` field from the record `sys.rt.info` builds.
    fn std_snapshot_field(context: &SysHostContext) -> Result<Value, EvaluationError> {
        let Value::Record(mut fields) = runtime_info(context)? else {
            panic!("sys.RuntimeInfo must be a record");
        };
        Ok(fields
            .remove("std_snapshot")
            .expect("sys.RuntimeInfo carries std_snapshot"))
    }

    #[test]
    fn unpinned_project_reports_no_standard_snapshot() {
        let context = context_pinned(StdSnapshotPin::Unpinned);
        let Ok(field) = std_snapshot_field(&context) else {
            panic!("an unpinned project is a valid sys.RuntimeInfo");
        };
        assert!(matches!(field, Value::Option(None)));
    }

    #[test]
    fn unreadable_pin_fails_closed_as_snapshot_incomplete() {
        let context = context_pinned(StdSnapshotPin::Unreadable);
        let Err(error) = std_snapshot_field(&context) else {
            panic!("an unreadable pin must not be guessed");
        };
        assert_eq!(error.code(), "sys.snapshot.incomplete");
    }

    #[test]
    fn sha1_pin_is_reported_as_a_snapshot_reference() {
        let context = context_pinned(StdSnapshotPin::Commit(vec![7; 20]));
        let Ok(field) = std_snapshot_field(&context) else {
            panic!("a 20-byte commit id is a valid pin");
        };
        let Value::Option(Some(reference)) = field else {
            panic!("a pinned standard commit must be `some`");
        };
        assert!(matches!(*reference, Value::Reference(_)));
    }

    #[test]
    fn commit_id_of_unknown_length_fails_closed() {
        let context = context_pinned(StdSnapshotPin::Commit(vec![7; 19]));
        let Err(error) = std_snapshot_field(&context) else {
            panic!("a commit id that is neither SHA-1 nor SHA-256 must not be guessed");
        };
        assert_eq!(error.code(), "sys.snapshot.incomplete");
    }
}
