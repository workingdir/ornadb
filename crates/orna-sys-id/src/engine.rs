//! Generated sealed engine-function identity from the engine catalogue.
//!
//! Every entry carries its durable 16-byte `FunctionId` bytes explicitly; they
//! are not derived from ordering. The registry above this crate maps each
//! variant to its behaviour in a match with no wildcard arm.

include!(concat!(env!("OUT_DIR"), "/engine_functions.rs"));
