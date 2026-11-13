//! Support code the `orna` command-line surfaces share: the committed table
//! schema identity, the source-file writer, the repository runtime identity, and
//! the percent decoder. These are the items the FUSE mount calls back into; they
//! hold no command dispatch and no user-facing diagnostics.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub mod encoding;
pub mod runtime_identity;
pub mod source_edit;
pub mod table_schema;

static REQUEST_NONCE: AtomicU64 = AtomicU64::new(1);

/// A process-unique nonce for a request: the clock, the process id and a
/// per-process counter, so two requests never share a nonce.
#[must_use]
pub fn request_nonce() -> u128 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    (now << 32)
        ^ (u128::from(std::process::id()) << 16)
        ^ u128::from(REQUEST_NONCE.fetch_add(1, Ordering::Relaxed))
}
