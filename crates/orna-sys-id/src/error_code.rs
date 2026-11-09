//! Generated closed failure-code vocabulary.
//!
//! The first [`PORTABLE_FAILURE_CODE_COUNT`] entries are fixed by `api/sys.json`;
//! the remaining entries are this engine's vocabulary and carry [`crate::Origin::Engine`].

include!(concat!(env!("OUT_DIR"), "/codes.rs"));
