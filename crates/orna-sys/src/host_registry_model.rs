//! Typed runtime host-operation registry decoded from the generated JSON.
//!
//! The build path keeps the string DTO in `orna-sys-model`; the runtime identity
//! is typed here, where the crate layer permits a dependency on `orna-sys-id`.

use std::collections::BTreeMap;

use orna_sys_id::{
    HostOperation, HostProvider, SysEffect, SysFailureCode, SysFunction, VersionedId,
};
use serde::{Deserialize, de};

pub use orna_sys_model::abi_version::AbiVersion;
pub use orna_sys_model::host_registry_model::{HostBindingDeclaration, HostProviderRole};

/// One typed host-operation contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostOperationDescriptor {
    pub name: HostOperation,
    pub version: AbiVersion,
    pub signature: String,
    pub parameters: Vec<String>,
    pub effects: Vec<SysEffect>,
    pub preconditions: Vec<String>,
    pub failures: Vec<SysFailureCode>,
    pub role: VersionedId,
    pub provider: HostProvider,
    pub implementation: String,
    /// The catalogue callable this host operation implements, when one exists.
    pub function: Option<SysFunction>,
}

#[derive(Deserialize)]
struct WireHostOperationDescriptor {
    name: String,
    version: AbiVersion,
    signature: String,
    parameters: Vec<String>,
    effects: Vec<String>,
    preconditions: Vec<String>,
    failures: Vec<String>,
    role: String,
    provider: String,
    implementation: String,
}

impl TryFrom<WireHostOperationDescriptor> for HostOperationDescriptor {
    type Error = String;

    fn try_from(wire: WireHostOperationDescriptor) -> Result<Self, Self::Error> {
        let name = HostOperation::from_label(&wire.name)
            .ok_or_else(|| format!("unknown host operation `{}`", wire.name))?;
        let effects = wire
            .effects
            .iter()
            .map(|text| {
                SysEffect::parse(text).ok_or_else(|| format!("unknown sys effect `{text}`"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let failures = wire
            .failures
            .iter()
            .map(|text| {
                SysFailureCode::parse(text)
                    .ok_or_else(|| format!("unknown sys failure code `{text}`"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let role = wire.role.parse().map_err(|error: orna_sys_id::IdError| {
            format!("invalid host role `{}`: {error}", wire.role)
        })?;
        let provider = HostProvider::parse(&wire.provider)
            .ok_or_else(|| format!("unknown host provider `{}`", wire.provider))?;
        let function = SysFunction::from_label(&wire.name);
        Ok(Self {
            name,
            version: wire.version,
            signature: wire.signature,
            parameters: wire.parameters,
            effects,
            preconditions: wire.preconditions,
            failures,
            role,
            provider,
            implementation: wire.implementation,
            function,
        })
    }
}

impl<'de> Deserialize<'de> for HostOperationDescriptor {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = WireHostOperationDescriptor::deserialize(deserializer)?;
        Self::try_from(wire).map_err(de::Error::custom)
    }
}

/// Typed, generated host-operation registry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemHostOperationRegistry {
    abi_version: AbiVersion,
    operations: BTreeMap<&'static str, HostOperationDescriptor>,
    roles: BTreeMap<String, HostProviderRole>,
    role_ids: BTreeMap<VersionedId, String>,
}

impl SystemHostOperationRegistry {
    pub fn from_parts(
        abi_version: AbiVersion,
        operations: Vec<HostOperationDescriptor>,
        roles: Vec<HostProviderRole>,
    ) -> Result<Self, String> {
        let mut operation_map = BTreeMap::new();
        for operation in operations {
            let name = operation.name.label();
            if operation_map.insert(name, operation).is_some() {
                return Err(format!("duplicate native host operation `{name}`"));
            }
        }
        let mut role_map = BTreeMap::new();
        let mut role_ids = BTreeMap::new();
        for role in roles {
            let name = role.name.clone();
            let id: VersionedId = name.parse().map_err(|error| {
                format!("native host role `{name}` is not a versioned id: {error}")
            })?;
            role_ids.insert(id, name.clone());
            if role_map.insert(name.clone(), role).is_some() {
                return Err(format!("duplicate native host role `{name}`"));
            }
        }
        Ok(Self {
            abi_version,
            operations: operation_map,
            roles: role_map,
            role_ids,
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

    /// Looks up a role by its parsed versioned identity, without re-rendering it.
    pub fn role_by_id(&self, id: &VersionedId) -> Option<&HostProviderRole> {
        self.role_ids.get(id).and_then(|name| self.roles.get(name))
    }

    pub fn roles(&self) -> impl Iterator<Item = &HostProviderRole> {
        self.roles.values()
    }

    /// Generate declaration stubs for the operations in one native provider role.
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
            let signature_prefix = format!("fn {}", operation.name.label());
            let tail = operation
                .signature
                .strip_prefix(&signature_prefix)
                .ok_or_else(|| {
                    format!(
                        "host operation `{}` has a mismatched signature",
                        operation.name.label()
                    )
                })?;
            let (module, local_name) =
                operation.name.label().rsplit_once('.').ok_or_else(|| {
                    format!("host operation `{}` has no module", operation.name.label())
                })?;
            if module.is_empty() || local_name.is_empty() || !tail.starts_with('(') {
                return Err(format!(
                    "host operation `{}` has an invalid signature",
                    operation.name.label()
                ));
            }
            declarations.push(HostBindingDeclaration {
                module: module.to_owned(),
                operation: operation.name.label().to_owned(),
                source: format!(
                    "// host-op: {}\npub fn {local_name}{tail} = error(code: \"sys.binding.stub\", message: \"generated host declaration\");\n",
                    operation.name.label()
                ),
            });
        }
        declarations.sort_by(|left, right| left.operation.cmp(&right.operation));
        Ok(declarations)
    }

    /// Decodes one serialized host-operation registry produced by the build.
    pub fn from_json(source: &str) -> Result<Self, serde_json::Error> {
        #[derive(Deserialize)]
        struct RawHostRegistry {
            abi_version: AbiVersion,
            operations: Vec<HostOperationDescriptor>,
            roles: Vec<HostProviderRole>,
        }

        let raw: RawHostRegistry = serde_json::from_str(source)?;
        Self::from_parts(raw.abi_version, raw.operations, raw.roles)
            .map_err(<serde_json::Error as serde::de::Error>::custom)
    }
}
