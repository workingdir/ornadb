//! The engine's `core.money.Money` registry entry and the currency identity it depends on (hir-host.md B6).
//!
//! Slice HB6 step 1 (recorded here, before any value is built): the old pipeline assigns a currency's object id from
//! the application's trusted `NominalDefinition` (`orna-evaluator/src/lib.rs`), and the conformance adapter derives that
//! id as `SHA-256(identity)[..16]` (`orna-conformance/src/semantic_adapter.rs::nominal_type_id`). The executor's
//! `orna_value::enum_type_id` uses domain separation and length prefixes, so it does not reproduce that id. The test
//! below records the difference; it is a stop condition for the slice, reported to `ornadb-csenl.38`.

#[cfg(test)]
mod tests {
    use sha2::{Digest as _, Sha256};

    /// The old adapter's derivation, copied from `orna-conformance/src/semantic_adapter.rs::nominal_type_id`.
    fn old_adapter_currency_id(identity: &str) -> [u8; 16] {
        Sha256::digest(identity.as_bytes())[..16]
            .try_into()
            .expect("truncated digest has the ObjectId width")
    }

    #[test]
    fn enum_type_id_does_not_reproduce_the_old_currency_id() {
        for identity in ["GBP", "core.money.GBP", "fixture.currencies.GBP"] {
            let derived = orna_value::enum_type_id(identity);
            let old = old_adapter_currency_id(identity);
            assert_ne!(
                derived, old,
                "enum_type_id({identity:?}) now equals the old adapter id; the HB6 stop condition no longer holds"
            );
        }
    }
}
