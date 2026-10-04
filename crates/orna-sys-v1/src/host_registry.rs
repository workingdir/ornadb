//! Portable metadata for the generated native host-operation registry.
//!
//! The operation descriptors are shared by semantic admission, the evaluator,
//! and the native provider implementations.

use std::collections::BTreeMap;

use serde::Deserialize;

const GENERATED_HOST_OPERATIONS: &str =
    include_str!(concat!(env!("OUT_DIR"), "/system_host_operations.json"));
const GENERATED_HOST_OPERATIONS_SCHEMA: &str = include_str!(concat!(
    env!("OUT_DIR"),
    "/system_host_operations.schema.json"
));

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct HostOperationDescriptor {
    pub name: String,
    pub version: super::AbiVersion,
    pub signature: String,
    #[serde(default)]
    pub parameters: Vec<String>,
    pub effects: Vec<String>,
    pub preconditions: Vec<String>,
    pub failures: Vec<String>,
    pub role: String,
    pub provider: String,
    pub implementation: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct HostProviderRole {
    pub name: String,
    pub version: super::AbiVersion,
    pub provider: String,
    pub effects: Vec<String>,
    pub operations: Vec<String>,
    pub required: bool,
}

#[derive(Deserialize)]
struct RawHostRegistry {
    abi_version: super::AbiVersion,
    operations: Vec<HostOperationDescriptor>,
    roles: Vec<HostProviderRole>,
}

/// Typed, generated operation and role metadata for built-in host bindings.
/// This private host-operation registry is separate from the frozen public
/// `api/sys.json` document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemHostOperationRegistry {
    abi_version: super::AbiVersion,
    operations: BTreeMap<String, HostOperationDescriptor>,
    roles: BTreeMap<String, HostProviderRole>,
}

impl SystemHostOperationRegistry {
    fn from_json(source: &str) -> Result<Self, serde_json::Error> {
        let raw: RawHostRegistry = serde_json::from_str(source)?;
        let operations = raw
            .operations
            .into_iter()
            .map(|operation| (operation.name.clone(), operation))
            .collect::<BTreeMap<_, _>>();
        let roles = raw
            .roles
            .into_iter()
            .map(|role| (role.name.clone(), role))
            .collect::<BTreeMap<_, _>>();
        Ok(Self {
            abi_version: raw.abi_version,
            operations,
            roles,
        })
    }

    pub fn abi_version(&self) -> super::AbiVersion {
        self.abi_version
    }

    pub fn operation(&self, name: &str) -> Option<&HostOperationDescriptor> {
        self.operations.get(name)
    }

    pub fn operations(&self) -> impl Iterator<Item = &HostOperationDescriptor> {
        self.operations.values()
    }

    pub fn role(&self, name: &str) -> Option<&HostProviderRole> {
        self.roles.get(name)
    }

    pub fn roles(&self) -> impl Iterator<Item = &HostProviderRole> {
        self.roles.values()
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
