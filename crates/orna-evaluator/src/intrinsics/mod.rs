//! Standard-library intrinsic dispatch, one file per module family.
//!
//! Stage S6 moves the per-kind handlers out of `lib.rs` as a pure motion: the
//! single exhaustive `IntrinsicId` match in `lib.rs` still decides the family
//! and calls the dispatch function here.

pub(crate) mod bits;
pub(crate) mod codecs;
pub(crate) mod collection;
pub(crate) mod concurrent;
pub(crate) mod money;
pub(crate) mod stats;
pub(crate) mod stream;
pub(crate) mod text;
pub(crate) mod time;
