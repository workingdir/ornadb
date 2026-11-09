//! Generated closed identity for the engine host ABI.
//!
//! Host operations are engine vocabulary, not members of the 1.1.0 portable
//! callable catalogue, so every entry carries [`crate::Origin::Engine`]. The set is
//! generated from `catalogue/engine-1.1.0.json`, which the build script pins by
//! SHA-256.

include!(concat!(env!("OUT_DIR"), "/hosts.rs"));
