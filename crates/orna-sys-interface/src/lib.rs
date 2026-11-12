//! The `sys` descriptor decoded from the hand-written `sys.orna` declarations.
//!
//! This crate is the single decoder of the native `sys` surface. `native_raw_document` derives the
//! [`RawSystemApi`] descriptor that the checker validates. The descriptor types are plain data with public fields,
//! so the checker's JSON guard can construct them from the retired catalogue for field-by-field comparison.

mod native_raw;

pub use native_raw::{
    NAMESPACE_FILES, SYS_ORNA, catalogue_class, declared_names, native_label, native_raw_document,
    render_generics, render_native, render_type,
};

/// `sys.orna` did not parse or did not decode into a descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidSysInterface;

#[derive(Debug, PartialEq)]
pub struct RawSystemApi {
    pub counts: RawCounts,
    pub singletons: Vec<RawSingleton>,
    pub opaque_identifiers: Vec<String>,
    pub reference_aliases: Vec<RawReferenceAlias>,
    pub value_types: Vec<RawValueType>,
    pub enums: std::collections::BTreeMap<String, Vec<String>>,
    pub relations: Vec<RawRelation>,
    pub functions: Vec<RawFunction>,
    pub failure_codes: Vec<String>,
    pub removed_names: std::collections::BTreeMap<String, RawRemovedName>,
}

#[derive(Debug, PartialEq)]
pub struct RawCounts {
    pub singletons: usize,
    pub opaque_identifiers: usize,
    pub reference_aliases: usize,
    pub value_types: usize,
    pub enums: usize,
    pub relations: usize,
    pub functions: usize,
    pub failure_codes: usize,
}

#[derive(Debug, PartialEq)]
pub struct RawSingleton {
    pub name: String,
    pub ty: String,
    pub availability: String,
}

#[derive(Debug, PartialEq)]
pub struct RawReferenceAlias {
    pub name: String,
    pub target: String,
    pub definition: String,
}

#[derive(Debug, PartialEq)]
pub struct RawValueType {
    pub name: String,
    pub kind: String,
    pub purpose: String,
    pub type_parameters: Vec<String>,
    pub fields: Vec<RawField>,
    pub invariants: Vec<String>,
}

#[derive(Debug, PartialEq)]
pub struct RawRelation {
    pub name: String,
    pub grouped_handle: String,
    pub kind: String,
    pub availability: String,
    pub key: String,
    pub purpose: String,
    pub key_fields: Vec<String>,
    pub invariants: Vec<String>,
    pub fields: Vec<RawField>,
    pub writable: bool,
    pub reference_type: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawField {
    pub name: String,
    pub ty: String,
}

#[derive(Debug, PartialEq)]
pub struct RawFunction {
    pub name: String,
    pub signature: String,
    pub effect: String,
    pub purpose: String,
    pub since: Option<String>,
    pub contract: Option<String>,
    pub preconditions: Option<String>,
    pub ownership: Option<String>,
    pub snapshot_rule: Option<String>,
}

#[derive(Debug, PartialEq)]
pub struct RawRemovedName {
    pub replacement: Option<String>,
    pub diagnostic: String,
}
