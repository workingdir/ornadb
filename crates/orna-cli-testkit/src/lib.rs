//! Test-only fixture builders shared by the `orna-cli` and `orna-fuse` test
//! targets: the format-3 repository and Git fixture builder, and the captured
//! media-row commit. Dev-dependency only; the shipped binary never links this.

pub mod format3;
pub mod media_commit;
