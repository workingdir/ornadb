//! Typed, generated identity for the Orna 1.1.0 `sys` surface.
//!
//! Every name in this crate is generated from the vendored specification catalogue
//! (`catalogue/sys-1.1.0.json`, a byte copy of `reference/Orna-1.1.0/api/sys.json`)
//! or from the engine-private inventory in `catalogue/engine-1.1.0.json`. Names exist
//! only at the parse and print boundaries; production code matches on the enums.
//!
//! The crate has no runtime dependencies so that both `orna-compat-v1` (below it) and
//! `orna-sys` (above it) can depend on it without a cycle. `serde_json` is a build and
//! test dependency only.
//!
//! # Origin
//!
//! [`Origin::Portable`] names are fixed by the 1.1.0 specification. [`Origin::Engine`]
//! names are this implementation's own vocabulary; the split is visible at the type
//! level so a specification comparison can never silently accept a vendor name
//! (ORNA-SYS-004).

pub mod descriptor;
pub mod dispatch;
pub mod engine;
pub mod engine_ids;
pub mod error_code;
pub mod function;
pub mod host_operation;
pub mod provider_links;
pub mod relation;
pub mod removed;
pub mod versioned;

use core::fmt;

pub use descriptor::{
    ENGINE_FAILURE_CODE_COUNT, ENGINE_FUNCTIONS, ENGINE_NAME_COUNT, ENGINE_ONLY_FUNCTION_COUNT,
    ENGINE_ONLY_TYPE_COUNT, ENGINE_RETIRED_FUNCTION_COUNT, ENGINE_SEALED_FUNCTION_COUNT,
    ENGINE_TEST_FIXTURE_NAME_COUNT, ENGINE_TEST_FIXTURE_NAMES, ENGINE_TYPES,
    EngineFunctionDescriptor, EngineSignature, EngineTypeDescriptor, RETIRED_ENGINE_FUNCTIONS,
    RetiredEngineFunction, SYS_API_LANGUAGE_VERSION, SYS_API_SYS_VERSION, SysCallableDescriptor,
    SysEnumDescriptor, SysFailureCodeDescriptor, SysRelationDescriptor, SysRelationField,
    SysSingletonDescriptor, SysTypeDescriptor,
};
pub use dispatch::{SysFunctionBinding, SysFunctionHost, dispatch};
pub use engine::{ENGINE_FUNCTION_COUNT, EngineFunction};
pub use error_code::{PORTABLE_FAILURE_CODE_COUNT, SYS_FAILURE_CODE_COUNT, SysFailureCode};
pub use function::{AmbiguousPath, SYS_FUNCTION_COUNT, SysFunction};
pub use host_operation::{
    HOST_OPERATION_COUNT, HOST_OPERATION_DESCRIPTORS, HOST_PROVIDER_COUNT, HostOperation,
    HostOperationDescriptor, HostProvider, host_operation_descriptor,
};
pub use provider_links::{
    ProviderHostKind, SYS_FUNCTION_PROVIDER_HOSTS, SYS_GROUPED_HANDLE_PROVIDER_HOSTS,
    SYS_RELATION_PROVIDER_HOSTS, SYS_SINGLETON_PROVIDER_HOSTS,
};
pub use relation::{
    SYS_ENUM_COUNT, SYS_ENUM_VALUE_COUNT, SYS_FIELD_COUNT, SYS_GROUPED_HANDLE_COUNT,
    SYS_RELATION_COUNT, SYS_SINGLETON_COUNT, SYS_TYPE_COUNT, SysEnum, SysEnumValue, SysField,
    SysGroupedHandle, SysRelation, SysSingleton, SysType, SysTypeKind,
};
pub use removed::{
    DiagnosticIdentity, ORNA100_E_SYS_RUNTIME, PortableRemoval, RuleCode, SYS_REMOVED_NAME_COUNT,
    SysMemberPath, SysRemovedName,
};
pub use versioned::{AbiVersion, IdError, Namespace, VersionedId};

/// Whether a closed identity name is fixed by the specification or by this engine.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Origin {
    /// Fixed by the Orna 1.1.0 portable `sys` catalogue.
    Portable,
    /// This implementation's own vocabulary, absent from the specification.
    Engine,
}

impl Origin {
    /// Every origin.
    pub const ALL: [Origin; 2] = [Origin::Portable, Origin::Engine];

    /// The lowercase spelling used in diagnostics and evidence.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Origin::Portable => "portable",
            Origin::Engine => "engine",
        }
    }

    /// The exact inverse of [`Self::as_str`].
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "portable" => Some(Origin::Portable),
            "engine" => Some(Origin::Engine),
            _ => None,
        }
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The effect a catalogued callable declares.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SysEffect {
    /// A read that does not change durable state.
    Read,
    /// An invocation that may change state.
    Invoke,
    /// An administrative operation.
    Admin,
}

impl SysEffect {
    /// Every effect.
    pub const ALL: [SysEffect; 3] = [SysEffect::Read, SysEffect::Invoke, SysEffect::Admin];

    /// The exact `api/sys.json` spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            SysEffect::Read => "read",
            SysEffect::Invoke => "invoke",
            SysEffect::Admin => "admin",
        }
    }

    /// The exact inverse of [`Self::as_str`].
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "read" => Some(SysEffect::Read),
            "invoke" => Some(SysEffect::Invoke),
            "admin" => Some(SysEffect::Admin),
            _ => None,
        }
    }

    /// The Rust variant identifier, for typed declarations.
    #[must_use]
    pub const fn variant_name(self) -> &'static str {
        match self {
            SysEffect::Read => "Read",
            SysEffect::Invoke => "Invoke",
            SysEffect::Admin => "Admin",
        }
    }

    /// Resolves a variant identifier used by a typed declaration.
    #[must_use]
    pub fn from_variant_name(name: &str) -> Option<Self> {
        match name {
            "Read" => Some(SysEffect::Read),
            "Invoke" => Some(SysEffect::Invoke),
            "Admin" => Some(SysEffect::Admin),
            _ => None,
        }
    }
}

impl PartialEq<&str> for SysEffect {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl fmt::Display for SysEffect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The frozen specification coordinates the tables were generated from.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SysVersion {
    language: &'static str,
    sys: &'static str,
}

impl SysVersion {
    /// Constructs a coordinate pair.
    #[must_use]
    pub const fn new(language: &'static str, sys: &'static str) -> Self {
        Self { language, sys }
    }

    /// The Orna language version (`api/sys.json` `language_version`).
    #[must_use]
    pub const fn language(self) -> &'static str {
        self.language
    }

    /// The portable `sys` surface version (`api/sys.json` `sys_version`).
    #[must_use]
    pub const fn sys(self) -> &'static str {
        self.sys
    }
}

/// The reviewed 1.1.0 coordinates.
pub const SYS_VERSION: SysVersion = SysVersion::new(SYS_API_LANGUAGE_VERSION, SYS_API_SYS_VERSION);

/// The error returned when a name is not a member of a closed identity set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnknownSysMember {
    kind: &'static str,
    text: Box<str>,
}

impl UnknownSysMember {
    /// Records an unknown spelling of a `kind`-shaped name.
    #[must_use]
    pub fn new(kind: &'static str, text: &str) -> Self {
        Self {
            kind,
            text: text.into(),
        }
    }

    /// The identity set the name was looked up in.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        self.kind
    }

    /// The rejected spelling.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

impl fmt::Display for UnknownSysMember {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown {} `{}`", self.kind, self.text)
    }
}

impl std::error::Error for UnknownSysMember {}
