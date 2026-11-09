//! Small shared types and parsers used by both `orna-sys` and its build code.
//!
//! The build script and the library previously shared sources through
//! `#[path]` includes. Keeping the genuinely shared items in one crate gives
//! both targets one definition without compiling build-only code into the
//! library.

pub mod abi_version;
pub mod host_registry_model;
pub mod signature;
