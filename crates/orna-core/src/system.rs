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

use crate::{FunctionId, catalogue::QualifiedSemanticName};

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

const CATALOGUE_HEALTH_NAME_PARTS: &[&str] = &["sys", "catalog", "health"];
const SYS_INVOKE_NAME_PARTS: &[&str] = &["sys", "invoke"];

/// The behaviour selected for one sealed system function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SystemFunctionKind {
    /// Verifies the complete active database and protected security state.
    Health,
    /// Plans and coordinates one authenticated root invocation.
    Invoke,
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
}
