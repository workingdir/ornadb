//! Compile and run the actual live host modules without the legacy server
//! package's embedded PostgreSQL binary dependency.
//!
//! These paths intentionally point at production sources. The module unit
//! tests inside orna-server/src/live.rs and live_eval.rs therefore execute
//! unchanged in this conformance integration crate.

// These production modules keep items that orna-server itself exercises but
// this conformance binary does not reach; dead-code analysis is per-crate.
#[allow(dead_code)]
#[path = "../../orna-server/src/live_eval.rs"]
mod live_eval;

#[allow(dead_code)]
#[path = "../../orna-server/src/live.rs"]
mod live;
