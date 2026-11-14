//! Test-only fixture builders shared by the `orna-cli` and `orna-fuse` test
//! targets: the format-3 repository and Git fixture builder, and the captured
//! media-row commit. Dev-dependency only; the shipped binary never links this.

// Every helper here is test-only fixture support: a failed fixture step panics by
// design, and the helpers return values the caller consumes. Per-function `# Panics`
// sections and `#[must_use]` attributes on ~40 such helpers add no information.
#![allow(clippy::missing_panics_doc, clippy::must_use_candidate)]

pub mod format3;
pub mod media_commit;
