//! Runtime accessors for build-generated native host metadata and declarations.

use serde::Deserialize;

use crate::{AbiVersion, HostOperationDescriptor, HostProviderRole, SystemHostOperationRegistry};

const GENERATED_HOST_OPERATIONS: &str =
    include_str!(concat!(env!("OUT_DIR"), "/system_host_operations.json"));
const GENERATED_HOST_OPERATIONS_SCHEMA: &str = include_str!(concat!(
    env!("OUT_DIR"),
    "/system_host_operations.schema.json"
));
const GENERATED_HOST_BINDING_STUBS: &str =
    include_str!(concat!(env!("OUT_DIR"), "/system_host_bindings.orna"));
const GENERATED_HOST_BINDING_MODULES: &str = include_str!(concat!(
    env!("OUT_DIR"),
    "/system_host_binding_modules.json"
));

#[derive(Deserialize)]
struct RawHostRegistry {
    abi_version: AbiVersion,
    operations: Vec<HostOperationDescriptor>,
    roles: Vec<HostProviderRole>,
}

impl SystemHostOperationRegistry {
    fn from_json(source: &str) -> Result<Self, serde_json::Error> {
        let raw: RawHostRegistry = serde_json::from_str(source)?;
        Self::from_parts(raw.abi_version, raw.operations, raw.roles)
            .map_err(<serde_json::Error as serde::de::Error>::custom)
    }
}

pub fn system_host_operation_registry() -> &'static SystemHostOperationRegistry {
    static REGISTRY: std::sync::LazyLock<SystemHostOperationRegistry> =
        std::sync::LazyLock::new(|| {
            SystemHostOperationRegistry::from_json(GENERATED_HOST_OPERATIONS)
                .expect("build-validated generated sys host-operation registry")
        });
    &REGISTRY
}

/// Exact canonical bytes embedded from the build-time host-operation registry.
pub fn system_host_operation_registry_json() -> &'static str {
    GENERATED_HOST_OPERATIONS
}

/// Deterministic JSON Schema embedded alongside the generated host registry.
pub fn system_host_operation_registry_schema_json() -> &'static str {
    GENERATED_HOST_OPERATIONS_SCHEMA
}

/// Deterministic consumer-facing Orna declarations generated from the typed
/// native host-operation registry. The declaration bodies are error stubs;
/// execution remains in the registered Rust providers.
pub fn system_host_binding_stubs() -> &'static str {
    GENERATED_HOST_BINDING_STUBS
}

/// Canonical JSON map of generated native host module paths to their Orna
/// declarations, matching the module files emitted in Cargo's `OUT_DIR`.
pub fn system_host_binding_modules_json() -> &'static str {
    GENERATED_HOST_BINDING_MODULES
}
