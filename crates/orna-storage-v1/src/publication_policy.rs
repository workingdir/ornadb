//! Effective policy for compact publication batches.
//!
//! This value describes the compact writer's target and per-file bound. It is
//! a policy model; constructing it does not itself batch or write files.

use std::fmt;

const MIB: u64 = 1024 * 1024;

/// Default compressed-data target for a compact publication batch (16 MiB).
pub const DEFAULT_COMPRESSED_TARGET_BYTES: u64 = 16 * MIB;
/// Lowest supported compact publication target (8 MiB).
pub const MIN_COMPRESSED_TARGET_BYTES: u64 = 8 * MIB;
/// Highest supported compact publication target (32 MiB).
pub const MAX_COMPRESSED_TARGET_BYTES: u64 = 32 * MIB;
/// Maximum size of one file in the compact profile (64 MiB).
pub const COMPACT_FILE_BOUND_BYTES: u64 = 64 * MIB;

/// The effective target and hard per-file bound for compact publication.
///
/// The target applies to compressed data across a complete batch. It does not
/// define row or checkpoint semantics, and changing it does not change the
/// logical rows selected for publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompactPublicationPolicy {
    compressed_target_bytes: u64,
}

impl CompactPublicationPolicy {
    /// Creates policy with a caller-selected target within the normal range.
    pub fn new(compressed_target_bytes: u64) -> Result<Self, CompactPublicationPolicyError> {
        if !(MIN_COMPRESSED_TARGET_BYTES..=MAX_COMPRESSED_TARGET_BYTES)
            .contains(&compressed_target_bytes)
        {
            return Err(CompactPublicationPolicyError {
                requested_bytes: compressed_target_bytes,
            });
        }
        Ok(Self {
            compressed_target_bytes,
        })
    }

    /// Returns the effective compressed-data target in bytes.
    pub const fn compressed_target_bytes(self) -> u64 {
        self.compressed_target_bytes
    }

    /// Returns the compact profile's hard per-file size bound in bytes.
    pub const fn max_file_bytes(self) -> u64 {
        COMPACT_FILE_BOUND_BYTES
    }
}

impl Default for CompactPublicationPolicy {
    fn default() -> Self {
        Self {
            compressed_target_bytes: DEFAULT_COMPRESSED_TARGET_BYTES,
        }
    }
}

/// A compact publication target outside the supported tuning range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompactPublicationPolicyError {
    requested_bytes: u64,
}

impl CompactPublicationPolicyError {
    /// Returns the rejected target, in bytes.
    pub const fn requested_bytes(self) -> u64 {
        self.requested_bytes
    }
}

impl fmt::Display for CompactPublicationPolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "compressed publication target {} bytes is outside {}..={} bytes",
            self.requested_bytes, MIN_COMPRESSED_TARGET_BYTES, MAX_COMPRESSED_TARGET_BYTES
        )
    }
}

impl std::error::Error for CompactPublicationPolicyError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_exposes_effective_compact_policy() {
        let policy = CompactPublicationPolicy::default();
        assert_eq!(policy.compressed_target_bytes(), 16 * 1024 * 1024);
        assert_eq!(policy.max_file_bytes(), 64 * 1024 * 1024);
    }

    #[test]
    fn tuning_accepts_range_endpoints() {
        assert_eq!(
            CompactPublicationPolicy::new(MIN_COMPRESSED_TARGET_BYTES)
                .unwrap()
                .compressed_target_bytes(),
            MIN_COMPRESSED_TARGET_BYTES
        );
        assert_eq!(
            CompactPublicationPolicy::new(MAX_COMPRESSED_TARGET_BYTES)
                .unwrap()
                .compressed_target_bytes(),
            MAX_COMPRESSED_TARGET_BYTES
        );
    }

    #[test]
    fn tuning_rejects_values_outside_range() {
        assert_eq!(
            CompactPublicationPolicy::new(MIN_COMPRESSED_TARGET_BYTES - 1)
                .unwrap_err()
                .requested_bytes(),
            MIN_COMPRESSED_TARGET_BYTES - 1
        );
        assert_eq!(
            CompactPublicationPolicy::new(MAX_COMPRESSED_TARGET_BYTES + 1)
                .unwrap_err()
                .requested_bytes(),
            MAX_COMPRESSED_TARGET_BYTES + 1
        );
    }
}
