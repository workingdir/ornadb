//! Compact-format byte readers.
//!
//! The manifest reader decodes and validates the canonical compact manifest
//! and shard records from bytes alone. It does not know about Git, the working
//! tree, publication journals or the repository's path type: callers resolve
//! objects and convert the returned relative paths into their own types.

mod manifest;

pub use manifest::{
    COMPACT_MANIFEST_SHARD_LIMIT, COMPACT_PROFILE, ManifestHeader, SegmentRole, ShardEntry,
    ShardHeader, canonical_key_extrema, canonical_key_order, is_safe_atom, parse_manifest_header,
    parse_shard, validate_event_time_bounds,
};

use std::fmt;

/// A compact-format reader rejected its input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompactError {
    /// The bytes are not a canonical compact manifest, shard or segment record.
    InvalidManifest,
}

impl fmt::Display for CompactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidManifest => formatter.write_str("invalid compact manifest"),
        }
    }
}

impl std::error::Error for CompactError {}
