//! Native host environment operations in the generated sys binding registry.
//!
//! The host supplies an explicit allowlist (or a snapshot made from one); this
//! provider never enumerates the process environment or exposes mutation.

use std::collections::BTreeMap;

use orna_sys_macros::sys_host_operation;
use serde::Deserialize;

const GENERATED_HOST_OPERATIONS: &str =
    include_str!(concat!(env!("OUT_DIR"), "/system_host_operations.json"));

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct HostOperationDescriptor {
    pub name: String,
    pub version: super::AbiVersion,
    pub signature: String,
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

/// A validated snapshot of explicitly approved host environment names.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EnvironmentProvider {
    values: BTreeMap<String, Result<Option<String>, EnvironmentProviderError>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvironmentProviderError {
    InvalidName,
    Denied,
    Unset,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnvironmentDispatchValue {
    Optional(Option<String>),
    Required(String),
}

impl EnvironmentProviderError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidName => "sys.host.environment.invalid_name",
            Self::Denied => "sys.host.environment.denied",
            Self::Unset => "sys.host.environment.unset",
            Self::Unavailable => "sys.host.environment.unavailable",
        }
    }
}

impl EnvironmentProvider {
    /// Creates a deterministic host snapshot from the caller's allowlisted
    /// values. An absent option means the approved variable is unset.
    pub fn from_snapshot(
        values: impl IntoIterator<Item = (String, Option<String>)>,
    ) -> Result<Self, EnvironmentProviderError> {
        let mut approved = BTreeMap::new();
        for (name, value) in values {
            validate_environment_name(&name)?;
            if approved.insert(name, Ok(value)).is_some() {
                return Err(EnvironmentProviderError::InvalidName);
            }
        }
        Ok(Self { values: approved })
    }

    /// Captures only the supplied names from the process environment. Hosts
    /// remain responsible for excluding secret-bearing names from this list.
    pub fn capture_allowlisted(
        names: impl IntoIterator<Item = String>,
    ) -> Result<Self, EnvironmentProviderError> {
        let mut values = BTreeMap::new();
        for name in names {
            validate_environment_name(&name)?;
            let value = match std::env::var_os(&name) {
                None => Ok(None),
                Some(value) => value
                    .into_string()
                    .map(Some)
                    .map_err(|_| EnvironmentProviderError::Unavailable),
            };
            if values.insert(name, value).is_some() {
                return Err(EnvironmentProviderError::InvalidName);
            }
        }
        Ok(Self { values })
    }

    #[sys_host_operation(
        r###"{"name":"std.io.environment.get","version":{"major":1,"minor":0},"signature":"fn std.io.environment.get(name: Str): Str?","effects":["read"],"preconditions":["name matches [A-Z_][A-Z0-9_]*","name is explicitly allowlisted by the host"],"failures":["sys.host.environment.invalid_name","sys.host.environment.denied","sys.host.environment.unavailable"],"role":"host.std.io.environment@1.0","provider":"orna.sys.host.environment.v1","implementation":"get"}"###
    )]
    pub fn get(&self, name: &str) -> Result<Option<&str>, EnvironmentProviderError> {
        validate_environment_name(name)?;
        self.values
            .get(name)
            .ok_or(EnvironmentProviderError::Denied)?
            .as_ref()
            .map(|value| value.as_deref())
            .map_err(|error| *error)
    }

    #[sys_host_operation(
        r###"{"name":"std.io.environment.require","version":{"major":1,"minor":0},"signature":"fn std.io.environment.require(name: Str): Str","effects":["read"],"preconditions":["name matches [A-Z_][A-Z0-9_]*","name is explicitly allowlisted by the host"],"failures":["sys.host.environment.invalid_name","sys.host.environment.denied","sys.host.environment.unset","sys.host.environment.unavailable"],"role":"host.std.io.environment@1.0","provider":"orna.sys.host.environment.v1","implementation":"require"}"###
    )]
    pub fn require(&self, name: &str) -> Result<&str, EnvironmentProviderError> {
        self.get(name)?.ok_or(EnvironmentProviderError::Unset)
    }

    /// Invokes an operation selected from the generated typed host registry.
    /// The descriptor is rechecked against the embedded registry before its
    /// implementation selector reaches native code.
    pub fn dispatch(
        &self,
        operation: &HostOperationDescriptor,
        name: &str,
    ) -> Result<EnvironmentDispatchValue, EnvironmentProviderError> {
        let registered = system_host_operation_registry()
            .operation(&operation.name)
            .filter(|registered| *registered == operation)
            .ok_or(EnvironmentProviderError::Denied)?;
        let role = system_host_operation_registry()
            .role(&registered.role)
            .filter(|role| role.provider == registered.provider)
            .ok_or(EnvironmentProviderError::Denied)?;
        if !role
            .operations
            .iter()
            .any(|candidate| candidate == &registered.name)
            || !registered.effects.iter().any(|effect| effect == "read")
        {
            return Err(EnvironmentProviderError::Denied);
        }
        match registered.implementation.as_str() {
            "get" if registered.name == "std.io.environment.get" => self
                .get(name)
                .map(|value| EnvironmentDispatchValue::Optional(value.map(str::to_owned))),
            "require" if registered.name == "std.io.environment.require" => self
                .require(name)
                .map(|value| EnvironmentDispatchValue::Required(value.to_owned())),
            _ => Err(EnvironmentProviderError::Denied),
        }
    }
}

fn validate_environment_name(name: &str) -> Result<(), EnvironmentProviderError> {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return Err(EnvironmentProviderError::InvalidName);
    };
    if !(first.is_ascii_uppercase() || first == b'_')
        || !bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(EnvironmentProviderError::InvalidName);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_registry_matches_native_provider_methods() {
        let registry = system_host_operation_registry();
        let role = registry
            .role("host.std.io.environment@1.0")
            .expect("environment provider role is registered");
        assert_eq!(role.provider, "orna.sys.host.environment.v1");
        assert_eq!(role.operations.len(), 2);
        assert_eq!(
            registry
                .operation("std.io.environment.get")
                .map(|operation| operation.implementation.as_str()),
            Some("get")
        );
        assert_eq!(
            registry
                .operation("std.io.environment.require")
                .map(|operation| operation.implementation.as_str()),
            Some("require")
        );
    }

    #[test]
    fn environment_provider_enforces_allowlist_and_portable_names() {
        let provider = EnvironmentProvider::from_snapshot([
            ("APP_MODE".into(), Some("test".into())),
            ("OPTIONAL".into(), None),
        ])
        .unwrap();
        assert_eq!(provider.get("APP_MODE"), Ok(Some("test")));
        assert_eq!(provider.get("OPTIONAL"), Ok(None));
        assert_eq!(provider.get("HOME"), Err(EnvironmentProviderError::Denied));
        assert_eq!(
            provider.get("app_mode"),
            Err(EnvironmentProviderError::InvalidName)
        );
        assert_eq!(
            provider.require("OPTIONAL"),
            Err(EnvironmentProviderError::Unset)
        );
    }
}
