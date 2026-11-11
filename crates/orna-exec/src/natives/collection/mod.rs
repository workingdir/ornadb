//! The `core.collection` engine natives, one module per shape: whole-list natives, closure natives and keyed natives.
//! `bucket_by` is not registered yet, so a call to it stops the run as not yet ported.

pub(super) mod basic;
pub(super) mod closure;
pub(super) mod keyed;
