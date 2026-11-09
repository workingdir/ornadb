//! Retained Orna repository-format byte codecs.
//!
//! This crate is layer 7 ("formats and planning"). It owns the versioned
//! readers that decode retained format-2 bytes and the format-3 byte pieces
//! that do not need a repository, publication coordinator or Git transport.
//! Readers are handed bytes or an object view; they never resolve
//! `.orna/blobs/`, write rows or inspect the working tree.
//!
//! The stage-07 split keeps the byte-level stages here and leaves the
//! repository-owned admission and the publication coordinator in their
//! respective layers. The retained format-1/2 commits stay readable under
//! their original decoders (ORNA-ROW-013, ORNA-UPGRADE-001/002).

pub mod format2_ogb;
pub mod format2_orr;
pub mod format2_rov;
