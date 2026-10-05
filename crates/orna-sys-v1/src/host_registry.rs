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

/// One generated Orna declaration paired with its native registry operation.
/// The declaration is an ABI/type artifact; execution stays in the registered
/// Rust provider selected by the same operation metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostBindingDeclaration {
    pub module: String,
    pub operation: String,
    pub source: String,
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

    /// Generate declaration stubs for the operations in one native provider
    /// role. Their signatures and operation identities come only from the
    /// generated typed registry; native dispatch remains implemented by Rust.
    pub fn binding_declarations_for_role(
        &self,
        role_name: &str,
    ) -> Result<Vec<HostBindingDeclaration>, String> {
        let role = self
            .role(role_name)
            .ok_or_else(|| format!("unknown host provider role `{role_name}`"))?;
        let mut declarations = Vec::with_capacity(role.operations.len());
        for operation_name in &role.operations {
            let operation = self
                .operation(operation_name)
                .ok_or_else(|| format!("host role `{role_name}` references a missing operation"))?;
            let signature_prefix = format!("fn {}", operation.name);
            let tail = operation
                .signature
                .strip_prefix(&signature_prefix)
                .ok_or_else(|| {
                    format!(
                        "host operation `{}` has a mismatched signature",
                        operation.name
                    )
                })?;
            let (module, local_name) = operation
                .name
                .rsplit_once('.')
                .ok_or_else(|| format!("host operation `{}` has no module", operation.name))?;
            if module.is_empty() || local_name.is_empty() || !tail.starts_with('(') {
                return Err(format!(
                    "host operation `{}` has an invalid signature",
                    operation.name
                ));
            }
            declarations.push(HostBindingDeclaration {
                module: module.to_owned(),
                operation: operation.name.clone(),
                source: format!(
                    "// host-op: {}\npub fn {local_name}{tail} = error(code: \"sys.binding.stub\", message: \"generated host declaration\");\n",
                    operation.name
                ),
            });
        }
        declarations.sort_by(|left, right| left.operation.cmp(&right.operation));
        Ok(declarations)
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
