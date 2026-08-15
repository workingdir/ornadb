//! The sealed registry of mandatory ring-1 system functions.
//!
//! Work ADR 0042 defines one compiled registry in `orna-core` for functions
//! that must exist before an application catalogue is available. The registry
//! is not reconstructed from application source, the standard library,
//! PostgreSQL rows, environment values, or configuration. The first two
//! entries are catalogue health and the root invocation gateway.
//!
//! Name comparison uses the complete case-sensitive resolved parts. It does
//! not perform case folding, prefix matching, alias lookup, search-path
//! resolution, or application-catalogue fallback.
//!
//! Application catalogues remain separate from this registry. They cannot
//! replace a sealed definition by reusing its identity or exact name.

use crate::{FunctionId, TypeId, catalogue::QualifiedSemanticName};

/// The stable identity of the sealed `sys.catalog.health` function.
pub const CATALOGUE_HEALTH_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);

/// The exact resolved name of the sealed catalogue-health function.
pub const CATALOGUE_HEALTH_FUNCTION_NAME: &str = "sys.catalog.health";

/// The stable identity of the mandatory sealed `sys.invoke` function.
pub const SYS_INVOKE_FUNCTION_ID: FunctionId =
    FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2]);

/// The exact resolved name of the mandatory invocation gateway.
pub const SYS_INVOKE_FUNCTION_NAME: &str = "sys.invoke";

/// The stable identity of the sealed `sys.invoke.Value` carrier.
pub const SYS_INVOKE_VALUE_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf0]);

/// The exact semantic name of the sealed invocation-value carrier.
pub const SYS_INVOKE_VALUE_TYPE_NAME: &str = "sys.invoke.Value";

/// The immutable representation contract of the sealed invocation-value carrier.
pub const SYS_INVOKE_VALUE_REPRESENTATION_CONTRACT: &str = "orna.sys.invoke.value@1";

/// The stable identity of the sealed `sys.invoke.Request` carrier.
pub const SYS_INVOKE_REQUEST_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf1]);

/// The exact semantic name of the sealed invocation-request carrier.
pub const SYS_INVOKE_REQUEST_TYPE_NAME: &str = "sys.invoke.Request";

/// The immutable representation contract of the sealed invocation-request carrier.
pub const SYS_INVOKE_REQUEST_REPRESENTATION_CONTRACT: &str = "orna.sys.invoke.request@1";

/// The stable identity of the sealed `sys.invoke.Event` carrier.
pub const SYS_INVOKE_EVENT_TYPE_ID: TypeId =
    TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf2]);

/// The exact semantic name of the sealed invocation-event carrier.
pub const SYS_INVOKE_EVENT_TYPE_NAME: &str = "sys.invoke.Event";

/// The immutable representation contract of the sealed invocation-event carrier.
pub const SYS_INVOKE_EVENT_REPRESENTATION_CONTRACT: &str = "orna.sys.invoke.event@1";

const CATALOGUE_HEALTH_NAME_PARTS: &[&str] = &["sys", "catalog", "health"];
const SYS_INVOKE_NAME_PARTS: &[&str] = &["sys", "invoke"];
const SYS_INVOKE_VALUE_NAME_PARTS: &[&str] = &["sys", "invoke", "Value"];
const SYS_INVOKE_REQUEST_NAME_PARTS: &[&str] = &["sys", "invoke", "Request"];
const SYS_INVOKE_EVENT_NAME_PARTS: &[&str] = &["sys", "invoke", "Event"];

/// The behaviour selected for one sealed system function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SystemFunctionKind {
    /// Verifies the complete active database and protected security state.
    Health,
    /// Plans and coordinates one authenticated root invocation.
    Invoke,
}

/// The sealed invocation-carrier behaviour selected by one registry entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum InvocationCarrierKind {
    /// A typed invocation value.
    Value,
    /// A root invocation request.
    Request,
    /// A root invocation event.
    Event,
}

/// One immutable entry in the sealed invocation-carrier registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvocationCarrierDefinition {
    kind: InvocationCarrierKind,
    id: TypeId,
    name: &'static str,
    name_parts: &'static [&'static str],
    representation_contract: &'static str,
}

impl InvocationCarrierDefinition {
    const fn new(
        kind: InvocationCarrierKind,
        id: TypeId,
        name: &'static str,
        name_parts: &'static [&'static str],
        representation_contract: &'static str,
    ) -> Self {
        Self {
            kind,
            id,
            name,
            name_parts,
            representation_contract,
        }
    }

    /// Returns the sealed behaviour selected for this carrier.
    pub const fn kind(self) -> InvocationCarrierKind {
        self.kind
    }

    /// Returns this carrier's stable identity.
    pub const fn id(self) -> TypeId {
        self.id
    }

    /// Returns this carrier's exact case-sensitive semantic name.
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Returns the exact resolved qualified-name parts.
    pub const fn name_parts(self) -> &'static [&'static str] {
        self.name_parts
    }

    /// Returns this carrier's immutable representation contract.
    pub const fn representation_contract(self) -> &'static str {
        self.representation_contract
    }

    pub(crate) fn has_name(self, name: &QualifiedSemanticName) -> bool {
        name.parts()
            .iter()
            .map(String::as_str)
            .eq(self.name_parts.iter().copied())
    }
}

/// The complete sealed invocation-carrier registry in canonical order.
///
/// These definitions use fixed `TypeId` bytes. This registry does not allocate
/// an identity and cannot register an additional carrier at run time.
pub const INVOCATION_CARRIERS: &[InvocationCarrierDefinition] = &[
    InvocationCarrierDefinition::new(
        InvocationCarrierKind::Value,
        SYS_INVOKE_VALUE_TYPE_ID,
        SYS_INVOKE_VALUE_TYPE_NAME,
        SYS_INVOKE_VALUE_NAME_PARTS,
        SYS_INVOKE_VALUE_REPRESENTATION_CONTRACT,
    ),
    InvocationCarrierDefinition::new(
        InvocationCarrierKind::Request,
        SYS_INVOKE_REQUEST_TYPE_ID,
        SYS_INVOKE_REQUEST_TYPE_NAME,
        SYS_INVOKE_REQUEST_NAME_PARTS,
        SYS_INVOKE_REQUEST_REPRESENTATION_CONTRACT,
    ),
    InvocationCarrierDefinition::new(
        InvocationCarrierKind::Event,
        SYS_INVOKE_EVENT_TYPE_ID,
        SYS_INVOKE_EVENT_TYPE_NAME,
        SYS_INVOKE_EVENT_NAME_PARTS,
        SYS_INVOKE_EVENT_REPRESENTATION_CONTRACT,
    ),
];

/// Resolves one exact sealed invocation-carrier identity.
pub fn invocation_carrier_by_id(id: TypeId) -> Option<InvocationCarrierDefinition> {
    INVOCATION_CARRIERS
        .iter()
        .copied()
        .find(|definition| definition.id == id)
}

/// Resolves one exact sealed invocation-carrier name.
pub fn invocation_carrier_by_name(
    name: &QualifiedSemanticName,
) -> Option<InvocationCarrierDefinition> {
    INVOCATION_CARRIERS
        .iter()
        .copied()
        .find(|definition| definition.has_name(name))
}

/// One immutable entry in the sealed system-function registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SystemFunctionDefinition {
    kind: SystemFunctionKind,
    id: FunctionId,
    name_parts: &'static [&'static str],
}

impl SystemFunctionDefinition {
    const fn new(
        kind: SystemFunctionKind,
        id: FunctionId,
        name_parts: &'static [&'static str],
    ) -> Self {
        Self {
            kind,
            id,
            name_parts,
        }
    }

    /// Returns the sealed behaviour selected for this entry.
    pub const fn kind(self) -> SystemFunctionKind {
        self.kind
    }

    /// Returns this system function's stable identity.
    pub const fn id(self) -> FunctionId {
        self.id
    }

    /// Returns the exact resolved qualified-name parts.
    pub const fn name_parts(self) -> &'static [&'static str] {
        self.name_parts
    }

    pub(crate) fn has_name(self, name: &QualifiedSemanticName) -> bool {
        name.parts()
            .iter()
            .map(String::as_str)
            .eq(self.name_parts.iter().copied())
    }
}

/// The complete sealed system-function registry in canonical order.
pub const SYSTEM_FUNCTIONS: &[SystemFunctionDefinition] = &[
    SystemFunctionDefinition::new(
        SystemFunctionKind::Health,
        CATALOGUE_HEALTH_FUNCTION_ID,
        CATALOGUE_HEALTH_NAME_PARTS,
    ),
    SystemFunctionDefinition::new(
        SystemFunctionKind::Invoke,
        SYS_INVOKE_FUNCTION_ID,
        SYS_INVOKE_NAME_PARTS,
    ),
];

/// Resolves one exact sealed system-function identity.
pub fn system_function_by_id(id: FunctionId) -> Option<SystemFunctionDefinition> {
    SYSTEM_FUNCTIONS
        .iter()
        .copied()
        .find(|definition| definition.id == id)
}

/// Resolves one exact sealed system-function name.
pub fn system_function_by_name(name: &QualifiedSemanticName) -> Option<SystemFunctionDefinition> {
    SYSTEM_FUNCTIONS
        .iter()
        .copied()
        .find(|definition| definition.has_name(name))
}

#[cfg(test)]
mod tests {
    use crate::FunctionId;
    use crate::catalogue::QualifiedSemanticName;
    use crate::security::{CATALOGUE_HEALTH_FUNCTION_ID, CATALOGUE_HEALTH_FUNCTION_NAME};

    use super::*;

    fn qualified(parts: &[&str]) -> QualifiedSemanticName {
        QualifiedSemanticName::new(parts.iter().copied()).unwrap()
    }

    fn name_of(function: SystemFunctionDefinition) -> QualifiedSemanticName {
        qualified(function.name_parts())
    }

    fn parts_equal(actual: &[&str], expected: &[&str]) -> bool {
        actual.iter().copied().eq(expected.iter().copied())
    }

    #[test]
    fn system_registry_contains_exactly_the_two_sealed_entries_in_order() {
        assert_eq!(SYSTEM_FUNCTIONS.len(), 2);
        let health = SYSTEM_FUNCTIONS[0];
        assert_eq!(health.kind(), SystemFunctionKind::Health);
        assert_eq!(health.id(), CATALOGUE_HEALTH_FUNCTION_ID);
        assert!(parts_equal(
            health.name_parts(),
            &["sys", "catalog", "health"]
        ));
        let invoke = SYSTEM_FUNCTIONS[1];
        assert_eq!(invoke.kind(), SystemFunctionKind::Invoke);
        assert_eq!(invoke.id(), SYS_INVOKE_FUNCTION_ID);
        assert!(parts_equal(invoke.name_parts(), &["sys", "invoke"]));
    }

    #[test]
    fn system_function_lookup_returns_the_same_exact_definitions_by_id_and_name() {
        for &function in SYSTEM_FUNCTIONS {
            let by_id = system_function_by_id(function.id())
                .expect("the registry must resolve its own identity");
            let by_name = system_function_by_name(&name_of(function))
                .expect("the registry must resolve its own name");
            assert_eq!(by_id, function);
            assert_eq!(by_name, function);
            assert_eq!(by_id.kind(), function.kind());
            assert_eq!(by_name.id(), function.id());
        }
    }

    #[test]
    fn system_function_lookup_rejects_similar_prefix_case_unqualified_and_unknown_names() {
        for parts in [
            &["sys", "catalog", "healthx"][..],
            &["sys", "catalog"][..],
            &["Sys", "catalog", "health"][..],
            &["catalog", "health"][..],
            &["sys", "invok"][..],
            &["app", "unknown"][..],
        ] {
            assert!(
                system_function_by_name(&qualified(parts)).is_none(),
                "{parts:?}"
            );
        }
    }

    #[test]
    fn system_function_lookup_rejects_unknown_identities() {
        for bytes in [
            [0; 16],
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3],
            [0x7f; 16],
        ] {
            assert!(system_function_by_id(FunctionId::from_bytes(bytes)).is_none());
        }
    }

    #[test]
    fn catalogue_health_compatibility_facts_match_the_registry() {
        assert_eq!(CATALOGUE_HEALTH_FUNCTION_ID, SYSTEM_FUNCTIONS[0].id());
        assert_eq!(CATALOGUE_HEALTH_FUNCTION_NAME, "sys.catalog.health");
        assert_eq!(SYS_INVOKE_FUNCTION_NAME, "sys.invoke");
        assert_eq!(
            SYS_INVOKE_FUNCTION_ID,
            FunctionId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2])
        );
        assert_eq!(SYS_INVOKE_FUNCTION_ID, SYSTEM_FUNCTIONS[1].id());
    }

    #[test]
    fn invocation_carrier_registry_contains_the_three_fixed_entries_in_order() {
        assert_eq!(INVOCATION_CARRIERS.len(), 3);
        let expected = [
            (
                InvocationCarrierKind::Value,
                TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf0]),
                "sys.invoke.Value",
                "orna.sys.invoke.value@1",
            ),
            (
                InvocationCarrierKind::Request,
                TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf1]),
                "sys.invoke.Request",
                "orna.sys.invoke.request@1",
            ),
            (
                InvocationCarrierKind::Event,
                TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf2]),
                "sys.invoke.Event",
                "orna.sys.invoke.event@1",
            ),
        ];

        for (definition, (kind, id, name, contract)) in INVOCATION_CARRIERS.iter().zip(expected) {
            assert_eq!(definition.kind(), kind);
            assert_eq!(definition.id(), id);
            assert_eq!(definition.name(), name);
            assert_eq!(definition.representation_contract(), contract);
            assert_eq!(definition.name_parts().join("."), name);
        }
    }

    #[test]
    fn invocation_carrier_lookup_returns_exact_definitions_without_allocating_identities() {
        let expected = [
            (
                TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf0]),
                qualified(&["sys", "invoke", "Value"]),
            ),
            (
                TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf1]),
                qualified(&["sys", "invoke", "Request"]),
            ),
            (
                TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf2]),
                qualified(&["sys", "invoke", "Event"]),
            ),
        ];

        for (index, (id, name)) in expected.into_iter().enumerate() {
            let carrier = INVOCATION_CARRIERS[index];
            assert_eq!(invocation_carrier_by_id(id), Some(carrier));
            assert_eq!(invocation_carrier_by_name(&name), Some(carrier));
        }

        for id in [
            TypeId::from_bytes([0; 16]),
            TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xef]),
            TypeId::from_bytes([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xf3]),
        ] {
            assert_eq!(invocation_carrier_by_id(id), None);
        }
    }

    #[test]
    fn invocation_carrier_lookup_rejects_similar_and_unknown_names() {
        for parts in [
            &["sys", "invoke", "value"][..],
            &["sys", "invoke", "Request", "extra"][..],
            &["sys", "invoke"][..],
            &["invoke", "Event"][..],
            &["app", "Value"][..],
        ] {
            assert!(
                invocation_carrier_by_name(&qualified(parts)).is_none(),
                "{parts:?}"
            );
        }
    }
}
