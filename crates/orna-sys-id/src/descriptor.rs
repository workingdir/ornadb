//! Static descriptor tables and the types they carry.
//!
//! Every table is indexed by the enum discriminant, so lookup is one integer index
//! and no startup parse. The tables are generated into `OUT_DIR` by `build.rs`.

use crate::{
    Origin, SysEffect,
    error_code::{SYS_FAILURE_CODE_COUNT, SysFailureCode},
    function::{SYS_FUNCTION_COUNT, SysFunction},
    relation::{
        SYS_ENUM_COUNT, SYS_RELATION_COUNT, SYS_SINGLETON_COUNT, SYS_TYPE_COUNT, SysEnum,
        SysEnumValue, SysField, SysGroupedHandle, SysRelation, SysSingleton, SysType, SysTypeKind,
    },
};

/// Counts for the engine-private inventory.
pub const ENGINE_SEALED_FUNCTION_COUNT: usize = 27;
pub const ENGINE_ONLY_FUNCTION_COUNT: usize = 2;
pub const ENGINE_ONLY_TYPE_COUNT: usize = 4;
pub const ENGINE_RETIRED_FUNCTION_COUNT: usize = 2;
pub const ENGINE_FAILURE_CODE_COUNT: usize = 61;
pub const ENGINE_TEST_FIXTURE_NAME_COUNT: usize = 19;
pub const ENGINE_NAME_COUNT: usize = 115;

/// One catalogued callable's metadata, byte-for-byte from the catalogue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SysCallableDescriptor {
    pub function: SysFunction,
    pub label: &'static str,
    pub path: &'static str,
    pub overload: Option<&'static str>,
    pub signature: &'static str,
    pub effect: SysEffect,
    pub origin: Origin,
    pub purpose: &'static str,
    pub snapshot_rule: Option<&'static str>,
    pub ownership: Option<&'static str>,
    pub contract: Option<&'static str>,
    pub preconditions: Option<&'static str>,
    pub since: Option<&'static str>,
}

/// One failure code's wire bytes and origin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SysFailureCodeDescriptor {
    pub code: SysFailureCode,
    pub bytes: &'static str,
    pub origin: Origin,
}

/// One field declared by a relation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SysRelationField {
    pub field: SysField,
    pub type_name: &'static str,
}

/// One canonical relation's metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SysRelationDescriptor {
    pub relation: SysRelation,
    pub name: &'static str,
    pub grouped_handle: SysGroupedHandle,
    pub kind: &'static str,
    pub availability: &'static str,
    pub key: &'static str,
    pub reference_type: &'static str,
    pub purpose: &'static str,
    pub writable: bool,
    pub fields: &'static [SysRelationField],
}

/// One singleton's metadata and declared view fields.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SysSingletonDescriptor {
    pub singleton: SysSingleton,
    pub name: &'static str,
    pub view_type: &'static str,
    pub availability: &'static str,
    pub fields: &'static [SysField],
}

/// One enum type's declared constants.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SysEnumDescriptor {
    pub enumeration: SysEnum,
    pub name: &'static str,
    pub values: &'static [SysEnumValue],
}

/// One system type's metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SysTypeDescriptor {
    pub system_type: SysType,
    pub name: &'static str,
    pub kind: SysTypeKind,
    pub target: Option<&'static str>,
    pub purpose: Option<&'static str>,
    pub type_parameters: &'static [&'static str],
}

/// The sealed signature facts carried by an engine function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineSignature {
    /// `sys.catalog.health` carries no signature.
    Health,
    /// `sys.source.current` carries no signature.
    SourceIntrospection,
    /// The single `sys.invoke` gateway.
    Invocation {
        parameter_id: [u8; 16],
        parameter_name: &'static str,
        request_type: [u8; 16],
        request_required: bool,
        request_non_null: bool,
        event_stream_item_type: [u8; 16],
    },
    /// The `sys.inspect` snapshot, projection and trace shapes.
    Inspect {
        parameter_count: usize,
        parameter_types: &'static [[u8; 16]],
        result_type: Option<[u8; 16]>,
        stream_item_type: Option<[u8; 16]>,
    },
    /// The twelve `sys.security` shapes.
    Security {
        parameter_count: usize,
        returns_set: bool,
        returns_ref_principal: bool,
        returns_boolean: bool,
        stream_item_type: Option<[u8; 16]>,
    },
    /// The two `sys.state` shapes.
    State {
        parameter_count: usize,
        returns_set: bool,
    },
}

/// One engine function, sealed or engine-only.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineFunctionDescriptor {
    pub name: &'static str,
    pub name_parts: &'static [&'static str],
    pub function_id: Option<[u8; 16]>,
    pub sealed: bool,
    pub signature: Option<EngineSignature>,
}

/// One engine-only type carrier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineTypeDescriptor {
    pub name: &'static str,
    pub type_id: [u8; 16],
    pub representation_contract: &'static str,
}

/// One 1.0.0 function name retained for historical readers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetiredEngineFunction {
    pub name: &'static str,
    pub since: &'static str,
    pub signature: &'static str,
    pub reason: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/tables.rs"));

/// Looks up one callable descriptor by discriminant.
#[must_use]
pub fn sys_function_descriptor(function: SysFunction) -> &'static SysCallableDescriptor {
    &SYS_FUNCTION_DESCRIPTORS[function.index()]
}

/// Looks up one failure-code descriptor by discriminant.
#[must_use]
pub fn sys_failure_code_descriptor(code: SysFailureCode) -> &'static SysFailureCodeDescriptor {
    &SYS_FAILURE_CODE_DESCRIPTORS[code.index()]
}

/// Looks up one relation descriptor by discriminant.
#[must_use]
pub fn sys_relation_descriptor(relation: SysRelation) -> &'static SysRelationDescriptor {
    &SYS_RELATION_DESCRIPTORS[relation as usize]
}

/// Looks up one singleton descriptor by discriminant.
#[must_use]
pub fn sys_singleton_descriptor(singleton: SysSingleton) -> &'static SysSingletonDescriptor {
    &SYS_SINGLETON_DESCRIPTORS[singleton as usize]
}

/// Looks up one enum descriptor by discriminant.
#[must_use]
pub fn sys_enum_descriptor(enumeration: SysEnum) -> &'static SysEnumDescriptor {
    &SYS_ENUM_DESCRIPTORS[enumeration as usize]
}

/// Looks up one type descriptor by discriminant.
#[must_use]
pub fn sys_type_descriptor(system_type: SysType) -> &'static SysTypeDescriptor {
    &SYS_TYPE_DESCRIPTORS[system_type as usize]
}

/// Looks up an engine function by its exact spelling.
#[must_use]
pub fn engine_function(name: &str) -> Option<&'static EngineFunctionDescriptor> {
    ENGINE_FUNCTIONS.iter().find(|entry| entry.name == name)
}

/// Looks up an engine-only type by its exact spelling.
#[must_use]
pub fn engine_type(name: &str) -> Option<&'static EngineTypeDescriptor> {
    ENGINE_TYPES.iter().find(|entry| entry.name == name)
}
