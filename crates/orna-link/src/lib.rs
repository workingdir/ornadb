//! The link plan for Orna native declarations: the project's native-library
//! manifest, the digest check of each library file, and the binding of every
//! `native fn` to the engine registry or to a library (docs/design/native-libraries.md).
//!
//! This crate loads nothing and calls nothing. A plan is built before
//! evaluation; any error stops the run.

#![forbid(unsafe_code)]

mod error;
mod file;
mod manifest;
mod plan;

pub use error::{DeclSite, LinkError, LinkErrorCode};
pub use manifest::{LibraryTarget, MANIFEST_FILE, NativeLibrary, ProjectManifest, host_target};
pub use plan::{
    Binding, EngineRegistry, LIBRARY_NATIVE_ABI, LinkPlan, LinkProfile, PlannedLibrary, plan,
};
