//! The engine's `core.money.Money` registry entry (hir-host.md B6): which declaration is the `Money` value, and the
//! currency a `Money` value names.
//!
//! The currency of a value is the 16-byte id the host gives the currency's module-qualified declaration name
//! (`Host::nominal_type_id`). The default is `orna_value::enum_type_id`; the application may override it with its
//! database-scoped identity. Step 1 of HB6 found that the default does not equal the old conformance adapter's
//! `SHA-256(identity)`, and no golden bytes exist to demand it; the test below pins the default and records that
//! difference.

use orna_hir::{DefId, DefKind, Program, Ty, TyKind};

/// Whether `def` is the engine's `core.money.Money` type, the declaration the `Money` value is registered under.
pub(crate) fn is_money_type(program: &Program, def: DefId) -> bool {
    let definition = &program.defs[def];
    matches!(definition.kind, DefKind::Type(_))
        && definition.name.as_str() == "Money"
        && program.modules[definition.module].path.to_string() == "core.money"
}

/// The module-qualified declaration name of the currency `ty` names (`Money<C>`'s `C`), or `None` when `ty` is not a
/// nominal type. A declaration in the unnamed root module keeps its bare name.
pub(crate) fn currency_name(program: &Program, ty: Ty) -> Option<String> {
    let Some(TyKind::Adt(def, _)) = program.tys.kind(ty) else {
        return None;
    };
    let definition = &program.defs[*def];
    let module = program.modules[definition.module].path.to_string();
    Some(orna_value::qualified_name(
        Some(&module),
        definition.name.as_str(),
    ))
}

#[cfg(test)]
mod tests {
    use sha2::{Digest as _, Sha256};

    use crate::host::{Host, StoreHost};

    /// The old conformance adapter's derivation (`orna-conformance/src/semantic_adapter.rs::nominal_type_id`).
    fn old_adapter_currency_id(identity: &str) -> [u8; 16] {
        Sha256::digest(identity.as_bytes())[..16]
            .try_into()
            .expect("truncated digest has the ObjectId width")
    }

    #[test]
    fn the_host_default_currency_id_is_enum_type_id_of_the_qualified_name() {
        let host = StoreHost::empty();
        for name in ["main.GBP", "core.money.GBP", "GBP"] {
            assert_eq!(
                host.nominal_type_id(name),
                orna_value::enum_type_id(name),
                "the default must derive from the module-qualified name {name:?}"
            );
            assert_ne!(
                host.nominal_type_id(name),
                old_adapter_currency_id(name),
                "the default does not reproduce the old adapter id (step-1 finding, pinned)"
            );
        }
    }
}
