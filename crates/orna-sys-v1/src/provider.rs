//! Typed contracts and role linkage for the baked `sys` provider ABI.
//!
//! This is the host-side provider protocol. It does not load Wasm components
//! or expose a native plug-in loader; portable extension loading remains a
//! separate WIT/Component Model slice.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::{SystemEffect, TypedValue};

const GENERATED_PROVIDER_ABI: &str =
    include_str!(concat!(env!("OUT_DIR"), "/system_provider_abi.json"));
const PROVIDER_FAILURE_CODES: [&str; 3] = [
    "sys.abi.precondition_failed",
    "sys.abi.unavailable",
    "sys.abi.provider_failed",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Deserialize)]
pub struct AbiVersion {
    pub major: u16,
    pub minor: u16,
}

impl AbiVersion {
    pub const V1_0: Self = Self { major: 1, minor: 0 };

    pub const fn compatible_provider(self, provider: Self) -> bool {
        self.major == provider.major && provider.minor >= self.minor
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct OperationId(String);

impl OperationId {
    pub fn new(value: impl Into<String>) -> Result<Self, ProviderAbiError> {
        let value = value.into();
        if !valid_operation_id(&value) {
            return Err(ProviderAbiError::InvalidOperationId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct SemanticRoleId(String);

impl SemanticRoleId {
    pub fn new(value: impl Into<String>) -> Result<Self, ProviderAbiError> {
        let value = value.into();
        if !valid_qualified_id(&value) {
            return Err(ProviderAbiError::InvalidRoleId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ProviderId(String);

impl ProviderId {
    pub fn new(value: impl Into<String>) -> Result<Self, ProviderAbiError> {
        let value = value.into();
        if !valid_qualified_id(&value) {
            return Err(ProviderAbiError::InvalidProviderId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn valid_qualified_id(value: &str) -> bool {
    !value.is_empty()
        && value.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        })
}

fn valid_operation_id(value: &str) -> bool {
    let suffix_start = value.find(['(', '<']).unwrap_or(value.len());
    let name = &value[..suffix_start];
    if !valid_qualified_id(name) {
        return false;
    }
    let suffix = &value[suffix_start..];
    if suffix.is_empty() {
        return true;
    }
    let (open, close) = if suffix.starts_with('(') {
        ('(', ')')
    } else {
        ('<', '>')
    };
    if !suffix.ends_with(close) {
        return false;
    }
    let mut depth = 0usize;
    for character in suffix.chars() {
        if character == open {
            depth += 1;
        } else if character == close {
            let Some(next) = depth.checked_sub(1) else {
                return false;
            };
            depth = next;
        } else if !(character.is_ascii_alphanumeric()
            || matches!(
                character,
                '.' | '_' | '-' | ',' | ' ' | '[' | ']' | '?' | '<' | '>'
            ))
        {
            return false;
        }
    }
    depth == 0
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AbiType {
    Named(String),
    Applied {
        constructor: String,
        arguments: Vec<Self>,
    },
    List(Box<Self>),
    Optional(Box<Self>),
}

impl AbiType {
    pub fn canonical(&self) -> String {
        match self {
            Self::Named(name) => name.clone(),
            Self::Applied {
                constructor,
                arguments,
            } => format!(
                "{constructor}<{}>",
                arguments
                    .iter()
                    .map(Self::canonical)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::List(element) => format!("[{}]", element.canonical()),
            Self::Optional(inner) => format!("{}?", inner.canonical()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationParameter {
    pub name: String,
    pub ty: AbiType,
    pub default: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionSignature {
    pub callable: String,
    pub type_parameters: Vec<String>,
    pub parameters: Vec<OperationParameter>,
    pub result: AbiType,
    pub source: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct EffectSet(BTreeSet<SystemEffect>);

impl EffectSet {
    pub fn new(effects: impl IntoIterator<Item = SystemEffect>) -> Self {
        Self(effects.into_iter().collect())
    }

    pub fn one(effect: SystemEffect) -> Self {
        Self(BTreeSet::from([effect]))
    }

    pub fn is_subset_of(&self, ceiling: &Self) -> bool {
        self.0.is_subset(&ceiling.0)
    }

    pub fn iter(&self) -> impl Iterator<Item = SystemEffect> + '_ {
        self.0.iter().copied()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Precondition {
    /// Existing 1.0 preconditions are retained as stable declarations. Later
    /// ABI revisions may replace these descriptions with executable predicates.
    pub declaration: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct FailureCode(String);

impl FailureCode {
    pub fn new(value: impl Into<String>) -> Result<Self, ProviderAbiError> {
        let value = value.into();
        if !valid_qualified_id(&value) {
            return Err(ProviderAbiError::InvalidFailureCode);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationContract {
    pub id: OperationId,
    pub version: AbiVersion,
    pub signature: FunctionSignature,
    pub effects: EffectSet,
    pub preconditions: Vec<Precondition>,
    pub failures: BTreeSet<FailureCode>,
    pub role: Option<SemanticRoleId>,
    pub role_version: Option<AbiVersion>,
}

impl OperationContract {
    pub fn declares_failure(&self, code: &FailureCode) -> bool {
        self.failures.contains(code)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticRoleContract {
    pub id: SemanticRoleId,
    pub version: AbiVersion,
    pub effects: EffectSet,
    pub operations: Vec<OperationId>,
    pub required: bool,
    pub replaceable: bool,
    pub builtin_provider: Option<ProviderId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderOffer {
    pub provider: ProviderId,
    pub role: SemanticRoleId,
    pub version: AbiVersion,
    pub effects: EffectSet,
}

/// A provider implementation accepts only typed values and identifies its
/// claimed role/effect/version contract before it can be linked.
pub trait SystemOperationProvider: Send + Sync {
    fn offer(&self) -> &ProviderOffer;

    fn invoke(
        &self,
        operation: &OperationId,
        arguments: &[TypedValue],
    ) -> Result<TypedValue, ProviderFailure>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderFailure {
    pub code: FailureCode,
    pub payload: Option<TypedValue>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderAbiError {
    InvalidJson,
    InvalidOperationId,
    InvalidRoleId,
    InvalidProviderId,
    InvalidFailureCode,
    InvalidSignature,
    InvalidEffect,
    DuplicateOperation,
    DuplicateRole,
    RoleOperationMissing,
    OperationRoleMismatch,
    OperationRoleVersionMismatch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderDiagnostic {
    UnknownRole(SemanticRoleId),
    UnknownOperation(OperationId),
    DuplicateRoleContract(SemanticRoleId),
    RoleUnavailable {
        role: SemanticRoleId,
        version: AbiVersion,
    },
    DuplicateRoleProvider(SemanticRoleId),
    RoleVersionMismatch {
        role: SemanticRoleId,
        required: AbiVersion,
        provided: AbiVersion,
    },
    EffectIncompatible(SemanticRoleId),
    UndeclaredFailure {
        operation: OperationId,
        code: FailureCode,
    },
    ProviderNotExecutable(SemanticRoleId),
}

impl ProviderDiagnostic {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnknownRole(_) => "sys.abi.unknown_role",
            Self::UnknownOperation(_) => "sys.abi.unknown_operation",
            Self::DuplicateRoleContract(_) => "sys.abi.duplicate_role_contract",
            Self::RoleUnavailable { .. } => "sys.abi.role_unavailable",
            Self::DuplicateRoleProvider(_) => "sys.abi.duplicate_role_provider",
            Self::RoleVersionMismatch { .. } => "sys.abi.role_version_mismatch",
            Self::EffectIncompatible(_) => "sys.abi.effect_incompatible",
            Self::UndeclaredFailure { .. } => "sys.abi.undeclared_failure",
            Self::ProviderNotExecutable(_) => "sys.abi.provider_not_executable",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemProviderAbi {
    version: AbiVersion,
    operations: BTreeMap<OperationId, OperationContract>,
    roles: BTreeMap<SemanticRoleId, SemanticRoleContract>,
}

impl SystemProviderAbi {
    pub fn from_json(source: &str) -> Result<Self, ProviderAbiError> {
        let raw: RawAbi =
            serde_json::from_str(source).map_err(|_| ProviderAbiError::InvalidJson)?;
        let mut operations = BTreeMap::new();
        for raw_operation in raw.operations {
            let id = OperationId::new(raw_operation.name)?;
            let effect = parse_effect(&raw_operation.effect)?;
            let mut failures = raw_operation
                .failures
                .into_iter()
                .map(FailureCode::new)
                .collect::<Result<BTreeSet<_>, _>>()?;
            failures.extend(PROVIDER_FAILURE_CODES.iter().map(|code| {
                FailureCode::new(*code).expect("static provider failure code is valid")
            }));
            let role_annotation = raw_operation
                .role
                .as_deref()
                .map(parse_role_annotation)
                .transpose()?;
            let role = role_annotation
                .map(|(name, _)| SemanticRoleId::new(name))
                .transpose()?;
            let contract = OperationContract {
                id: id.clone(),
                version: raw_operation.version,
                signature: parse_signature(&raw_operation.signature)?,
                effects: EffectSet::one(effect),
                preconditions: raw_operation
                    .preconditions
                    .into_iter()
                    .map(|declaration| Precondition { declaration })
                    .collect(),
                failures,
                role,
                role_version: role_annotation.map(|(_, version)| version),
            };
            if operations.insert(id, contract).is_some() {
                return Err(ProviderAbiError::DuplicateOperation);
            }
        }
        let mut roles = BTreeMap::new();
        for raw_role in raw.roles {
            let id = SemanticRoleId::new(raw_role.name)?;
            let effects = raw_role
                .effects
                .into_iter()
                .map(|effect| parse_effect(&effect))
                .collect::<Result<Vec<_>, _>>()?;
            let role = SemanticRoleContract {
                id: id.clone(),
                version: raw_role.version,
                effects: EffectSet::new(effects),
                operations: raw_role
                    .operations
                    .into_iter()
                    .map(OperationId::new)
                    .collect::<Result<Vec<_>, _>>()?,
                required: raw_role.required,
                replaceable: raw_role.replaceable,
                builtin_provider: raw_role.builtin_provider.map(ProviderId::new).transpose()?,
            };
            if role.operations.is_empty()
                || role.operations.iter().any(|operation_id| {
                    operations
                        .get(operation_id)
                        .is_none_or(|operation| operation.role.as_ref() != Some(&id))
                })
            {
                return Err(ProviderAbiError::RoleOperationMissing);
            }
            if role.operations.iter().any(|operation_id| {
                operations
                    .get(operation_id)
                    .is_some_and(|operation| operation.role_version != Some(role.version))
            }) {
                return Err(ProviderAbiError::OperationRoleVersionMismatch);
            }
            if roles.insert(id, role).is_some() {
                return Err(ProviderAbiError::DuplicateRole);
            }
        }
        for operation in operations.values() {
            if let Some(role) = &operation.role
                && !roles.contains_key(role)
            {
                return Err(ProviderAbiError::OperationRoleMismatch);
            }
        }
        Ok(Self {
            version: raw.abi_version,
            operations,
            roles,
        })
    }

    pub fn version(&self) -> AbiVersion {
        self.version
    }

    pub fn operations(&self) -> impl Iterator<Item = &OperationContract> {
        self.operations.values()
    }

    pub fn operation(&self, id: &str) -> Option<&OperationContract> {
        self.operations.get(&OperationId(id.to_owned()))
    }

    /// Verifies a provider failure against the operation's declared vocabulary.
    pub fn validate_failure(
        &self,
        operation: &str,
        code: &FailureCode,
    ) -> Result<(), ProviderDiagnostic> {
        let id = OperationId::new(operation)
            .map_err(|_| ProviderDiagnostic::UnknownOperation(OperationId(operation.to_owned())))?;
        let contract = self
            .operations
            .get(&id)
            .ok_or_else(|| ProviderDiagnostic::UnknownOperation(id.clone()))?;
        if contract.declares_failure(code) {
            Ok(())
        } else {
            Err(ProviderDiagnostic::UndeclaredFailure {
                operation: id,
                code: code.clone(),
            })
        }
    }

    pub fn roles(&self) -> impl Iterator<Item = &SemanticRoleContract> {
        self.roles.values()
    }

    pub fn role(&self, id: &str) -> Option<&SemanticRoleContract> {
        self.roles.get(&SemanticRoleId(id.to_owned()))
    }

    pub fn validate(&self) -> Result<(), ProviderDiagnostic> {
        for role in self.roles.values() {
            if role.required && role.builtin_provider.is_none() {
                return Err(ProviderDiagnostic::RoleUnavailable {
                    role: role.id.clone(),
                    version: role.version,
                });
            }
            for operation_id in &role.operations {
                let Some(operation) = self.operations.get(operation_id) else {
                    return Err(ProviderDiagnostic::RoleUnavailable {
                        role: role.id.clone(),
                        version: role.version,
                    });
                };
                if !operation.effects.is_subset_of(&role.effects) {
                    return Err(ProviderDiagnostic::EffectIncompatible(role.id.clone()));
                }
            }
        }
        Ok(())
    }
}

pub fn system_provider_abi() -> &'static SystemProviderAbi {
    static ABI: std::sync::LazyLock<SystemProviderAbi> = std::sync::LazyLock::new(|| {
        SystemProviderAbi::from_json(GENERATED_PROVIDER_ABI)
            .expect("build-validated generated system provider ABI")
    });
    &ABI
}

/// Validates a provider claim against one semantic-role contract. Providers
/// must preserve the role version and may not widen its effect ceiling.
pub fn validate_provider_offer(
    contract: &SemanticRoleContract,
    offer: &ProviderOffer,
) -> Result<(), ProviderDiagnostic> {
    if offer.role != contract.id {
        return Err(ProviderDiagnostic::UnknownRole(offer.role.clone()));
    }
    if !contract.version.compatible_provider(offer.version) {
        return Err(ProviderDiagnostic::RoleVersionMismatch {
            role: contract.id.clone(),
            required: contract.version,
            provided: offer.version,
        });
    }
    if !offer.effects.is_subset_of(&contract.effects) {
        return Err(ProviderDiagnostic::EffectIncompatible(contract.id.clone()));
    }
    Ok(())
}

/// A deterministic one-provider-per-role link table. This phase exposes the
/// link contract; Wasm component discovery and executable loading are later.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderRoleRegistry {
    roles: BTreeMap<SemanticRoleId, SemanticRoleContract>,
    providers: BTreeMap<SemanticRoleId, ProviderOffer>,
}

impl ProviderRoleRegistry {
    pub fn from_baked_abi(abi: &SystemProviderAbi) -> Result<Self, ProviderDiagnostic> {
        abi.validate()?;
        let roles = abi
            .roles
            .iter()
            .map(|(id, role)| (id.clone(), role.clone()))
            .collect();
        let providers = abi
            .roles
            .values()
            .filter_map(|role| {
                role.builtin_provider.as_ref().map(|provider| {
                    (
                        role.id.clone(),
                        ProviderOffer {
                            provider: provider.clone(),
                            role: role.id.clone(),
                            version: role.version,
                            effects: role.effects.clone(),
                        },
                    )
                })
            })
            .collect();
        let registry = Self { roles, providers };
        registry.validate_required()?;
        Ok(registry)
    }

    pub fn with_roles(
        roles: impl IntoIterator<Item = SemanticRoleContract>,
    ) -> Result<Self, ProviderDiagnostic> {
        let mut contracts = BTreeMap::new();
        for role in roles {
            if contracts.insert(role.id.clone(), role.clone()).is_some() {
                return Err(ProviderDiagnostic::DuplicateRoleContract(role.id));
            }
        }
        Ok(Self {
            roles: contracts,
            providers: BTreeMap::new(),
        })
    }

    pub fn bind(&mut self, offer: ProviderOffer) -> Result<(), ProviderDiagnostic> {
        let contract = self
            .roles
            .get(&offer.role)
            .ok_or_else(|| ProviderDiagnostic::UnknownRole(offer.role.clone()))?;
        validate_provider_offer(contract, &offer)?;
        if let Some(current) = self.providers.get(&offer.role) {
            if current.provider == offer.provider {
                self.providers.insert(offer.role.clone(), offer);
                return Ok(());
            }
            if !contract.replaceable
                || contract.builtin_provider.as_ref() != Some(&current.provider)
            {
                return Err(ProviderDiagnostic::DuplicateRoleProvider(offer.role));
            }
        }
        self.providers.insert(offer.role.clone(), offer);
        Ok(())
    }

    pub fn resolve(&self, role: &str) -> Result<&ProviderOffer, ProviderDiagnostic> {
        let role_id = SemanticRoleId::new(role)
            .map_err(|_| ProviderDiagnostic::UnknownRole(SemanticRoleId(role.to_owned())))?;
        let contract = self
            .roles
            .get(&role_id)
            .ok_or_else(|| ProviderDiagnostic::UnknownRole(role_id.clone()))?;
        let Some(offer) = self.providers.get(&role_id) else {
            return Err(ProviderDiagnostic::RoleUnavailable {
                role: role_id,
                version: contract.version,
            });
        };
        validate_provider_offer(contract, offer)?;
        Ok(offer)
    }

    pub fn validate_required(&self) -> Result<(), ProviderDiagnostic> {
        for contract in self.roles.values().filter(|contract| contract.required) {
            self.resolve(contract.id.as_str())?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct RawAbi {
    abi_version: AbiVersion,
    operations: Vec<RawOperation>,
    roles: Vec<RawRole>,
}

#[derive(Deserialize)]
struct RawOperation {
    name: String,
    version: AbiVersion,
    signature: String,
    effect: String,
    preconditions: Vec<String>,
    failures: Vec<String>,
    role: Option<String>,
}

#[derive(Deserialize)]
struct RawRole {
    name: String,
    version: AbiVersion,
    effects: Vec<String>,
    operations: Vec<String>,
    required: bool,
    replaceable: bool,
    builtin_provider: Option<String>,
}

fn parse_effect(value: &str) -> Result<SystemEffect, ProviderAbiError> {
    match value {
        "read" => Ok(SystemEffect::Read),
        "invoke" => Ok(SystemEffect::Invoke),
        "admin" => Ok(SystemEffect::Admin),
        _ => Err(ProviderAbiError::InvalidEffect),
    }
}

fn parse_role_annotation(value: &str) -> Result<(&str, AbiVersion), ProviderAbiError> {
    let (name, version) = value
        .rsplit_once('@')
        .ok_or(ProviderAbiError::InvalidRoleId)?;
    let (major, minor) = version
        .split_once('.')
        .ok_or(ProviderAbiError::InvalidRoleId)?;
    let version = AbiVersion {
        major: major.parse().map_err(|_| ProviderAbiError::InvalidRoleId)?,
        minor: minor.parse().map_err(|_| ProviderAbiError::InvalidRoleId)?,
    };
    if !valid_qualified_id(name) {
        return Err(ProviderAbiError::InvalidRoleId);
    }
    Ok((name, version))
}

fn parse_signature(source: &str) -> Result<FunctionSignature, ProviderAbiError> {
    let signature = source
        .strip_prefix("fn ")
        .ok_or(ProviderAbiError::InvalidSignature)?;
    let open = signature
        .find('(')
        .ok_or(ProviderAbiError::InvalidSignature)?;
    let close = matching(signature, open, '(', ')').ok_or(ProviderAbiError::InvalidSignature)?;
    let header = signature[..open].trim();
    let (callable, type_parameters) = if let Some(generic_open) = header.find('<') {
        if !header.ends_with('>') {
            return Err(ProviderAbiError::InvalidSignature);
        }
        let parameters = split_top_level(&header[generic_open + 1..header.len() - 1], ',')?
            .into_iter()
            .map(str::to_owned)
            .collect();
        (header[..generic_open].to_owned(), parameters)
    } else {
        (header.to_owned(), Vec::new())
    };
    if !valid_qualified_id(&callable) {
        return Err(ProviderAbiError::InvalidSignature);
    }
    let result_source = signature[close + 1..]
        .trim()
        .strip_prefix(':')
        .ok_or(ProviderAbiError::InvalidSignature)?
        .trim();
    let mut parameters = Vec::new();
    for source in split_top_level(&signature[open + 1..close], ',')? {
        if source.trim().is_empty() {
            continue;
        }
        let (parameter_source, default) = match source.split_once(" = ") {
            Some((parameter, default)) => (parameter, Some(default.to_owned())),
            None => (source, None),
        };
        let (name, ty) = parameter_source
            .split_once(':')
            .ok_or(ProviderAbiError::InvalidSignature)?;
        let name = name.trim();
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(ProviderAbiError::InvalidSignature);
        }
        parameters.push(OperationParameter {
            name: name.to_owned(),
            ty: parse_type(ty.trim())?,
            default,
        });
    }
    Ok(FunctionSignature {
        callable,
        type_parameters,
        parameters,
        result: parse_type(result_source)?,
        source: source.to_owned(),
    })
}

fn parse_type(source: &str) -> Result<AbiType, ProviderAbiError> {
    let source = source.trim();
    if let Some(inner) = source.strip_suffix('?') {
        return Ok(AbiType::Optional(Box::new(parse_type(inner)?)));
    }
    if source.starts_with('[') && source.ends_with(']') {
        return Ok(AbiType::List(Box::new(parse_type(
            &source[1..source.len() - 1],
        )?)));
    }
    if let Some(open) = source.find('<') {
        if !source.ends_with('>') {
            return Err(ProviderAbiError::InvalidSignature);
        }
        let constructor = source[..open].trim();
        if !valid_qualified_id(constructor) {
            return Err(ProviderAbiError::InvalidSignature);
        }
        let arguments = split_top_level(&source[open + 1..source.len() - 1], ',')?
            .into_iter()
            .map(parse_type)
            .collect::<Result<Vec<_>, _>>()?;
        if arguments.is_empty() {
            return Err(ProviderAbiError::InvalidSignature);
        }
        return Ok(AbiType::Applied {
            constructor: constructor.to_owned(),
            arguments,
        });
    }
    if !valid_qualified_id(source) {
        return Err(ProviderAbiError::InvalidSignature);
    }
    Ok(AbiType::Named(source.to_owned()))
}

fn split_top_level(source: &str, delimiter: char) -> Result<Vec<&str>, ProviderAbiError> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut angles = 0usize;
    let mut squares = 0usize;
    for (index, character) in source.char_indices() {
        match character {
            '<' => {
                angles = angles
                    .checked_add(1)
                    .ok_or(ProviderAbiError::InvalidSignature)?
            }
            '>' => {
                angles = angles
                    .checked_sub(1)
                    .ok_or(ProviderAbiError::InvalidSignature)?
            }
            '[' => {
                squares = squares
                    .checked_add(1)
                    .ok_or(ProviderAbiError::InvalidSignature)?
            }
            ']' => {
                squares = squares
                    .checked_sub(1)
                    .ok_or(ProviderAbiError::InvalidSignature)?
            }
            value if value == delimiter && angles == 0 && squares == 0 => {
                result.push(source[start..index].trim());
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    if angles != 0 || squares != 0 {
        return Err(ProviderAbiError::InvalidSignature);
    }
    if !source[start..].trim().is_empty() {
        result.push(source[start..].trim());
    }
    Ok(result)
}

fn matching(source: &str, start: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, character) in source[start..].char_indices() {
        if character == open {
            depth = depth.checked_add(1)?;
        } else if character == close {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(start + offset);
            }
        }
    }
    None
}
