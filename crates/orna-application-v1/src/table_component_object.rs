//! Stable object identities for the `sys.Key` and `sys.Storage` components of
//! a committed table (ORNA-SYS-017, ORNA-SYS-018).
//!
//! A table's key and storage are separate semantic objects, so each needs an
//! object identity distinct from the table's own identity and from the other
//! component. The identity is derived only from the table's committed object
//! identity and the component kind. It does not depend on the table name, a
//! column name, or any other editable detail, so an ordinary edit or a semantic
//! rename of the table leaves it unchanged.

use orna_runtime_v1::TableObjectId;
use sha2::{Digest, Sha256};

/// Domain separator for derived table component identities. Versioned so a
/// future derivation rule cannot collide with this one.
const DOMAIN: &[u8] = b"orna.sys.table-component.v1";

/// The table components that carry their own `sys.Key` or `sys.Storage`
/// object identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TableComponent {
    Key,
    Storage,
}

impl TableComponent {
    /// The fixed kind label mixed into the derivation. Labels are constant and
    /// the table identity is fixed width, so the encoding is unambiguous.
    const fn label(self) -> &'static [u8] {
        match self {
            Self::Key => b"key",
            Self::Storage => b"storage",
        }
    }
}

/// Derives the sixteen-byte object identity of `component` for the table
/// `table`: the first sixteen bytes of
/// `SHA-256(DOMAIN || 0x00 || table || 0x00 || label)`.
pub(crate) fn table_component_object_id(
    table: TableObjectId,
    component: TableComponent,
) -> [u8; 16] {
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    digest.update([0x00]);
    digest.update(table.bytes());
    digest.update([0x00]);
    digest.update(component.label());
    let hash = digest.finalize();
    let mut id = [0u8; 16];
    id.copy_from_slice(&hash[..16]);
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(byte: u8) -> TableObjectId {
        TableObjectId::new([byte; 16])
    }

    #[test]
    fn key_and_storage_identities_are_distinct_from_each_other_and_the_table() {
        let table_id = table(7);
        let key = table_component_object_id(table_id, TableComponent::Key);
        let storage = table_component_object_id(table_id, TableComponent::Storage);
        assert_ne!(key, storage);
        assert_ne!(key, table_id.bytes());
        assert_ne!(storage, table_id.bytes());
    }

    #[test]
    fn derivation_is_deterministic_and_separates_tables() {
        assert_eq!(
            table_component_object_id(table(7), TableComponent::Key),
            table_component_object_id(table(7), TableComponent::Key),
        );
        assert_ne!(
            table_component_object_id(table(7), TableComponent::Key),
            table_component_object_id(table(8), TableComponent::Key),
        );
    }
}
