//! Typed host-operation registry shared by the build script and runtime.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::abi_version::AbiVersion;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HostOperationDescriptor {
    pub name: String,
    pub version: AbiVersion,
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HostProviderRole {
    pub name: String,
    pub version: AbiVersion,
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

/// Typed, generated operation and role metadata for built-in host bindings.
/// This private host-operation registry is separate from frozen `api/sys.json`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemHostOperationRegistry {
    abi_version: AbiVersion,
    operations: BTreeMap<String, HostOperationDescriptor>,
    roles: BTreeMap<String, HostProviderRole>,
}

impl SystemHostOperationRegistry {
    pub fn from_parts(
        abi_version: AbiVersion,
        operations: Vec<HostOperationDescriptor>,
        roles: Vec<HostProviderRole>,
    ) -> Result<Self, String> {
        let mut operation_map = BTreeMap::new();
        for operation in operations {
            let name = operation.name.clone();
            if operation_map.insert(name.clone(), operation).is_some() {
                return Err(format!("duplicate native host operation `{name}`"));
            }
        }
        let mut role_map = BTreeMap::new();
        for role in roles {
            let name = role.name.clone();
            if role_map.insert(name.clone(), role).is_some() {
                return Err(format!("duplicate native host role `{name}`"));
            }
        }
        Ok(Self {
            abi_version,
            operations: operation_map,
            roles: role_map,
        })
    }

    pub fn abi_version(&self) -> AbiVersion {
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
    /// role. Native dispatch remains implemented by Rust.
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
