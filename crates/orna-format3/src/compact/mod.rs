//! Compact-format byte readers.
//!
//! The manifest reader decodes and validates the canonical compact manifest
//! and shard records from bytes alone. The model owns the manifest,
//! projection and canonical path layout, and the page validator checks the
//! compact Parquet framing. None of this knows about Git, the working tree,
//! publication journals or the repository's managed path type.

mod manifest;
mod model;
mod page;

pub use manifest::{
    COMPACT_MANIFEST_SHARD_LIMIT, COMPACT_PROFILE, ManifestHeader, SegmentRole, ShardEntry,
    ShardHeader, canonical_key_extrema, canonical_key_order, is_safe_atom, parse_manifest_header,
    parse_shard, validate_event_time_bounds,
};
pub use model::{
    COMPACT_MAX_UNCOMPRESSED_PAGE_BYTES, CompactCommittedRow, CompactCommittedSegmentProjection,
    CompactManifest, CompactManifestEntry, CompactSegmentRole, data_root, is_safe_relative_path,
    manifest_path, segment_path, shard_path,
};
pub use page::{
    decode_compact_i16, decode_compact_i32, scan_compact_page_headers, skip_compact_value,
    take_compact_byte, validate_compact_page_uncompressed_sizes,
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
