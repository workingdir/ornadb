//! Stable private PostgreSQL storage names derived from opaque Orna identities.

use orna_core::{FieldId, TypeId};

pub(crate) const DATA_SCHEMA: &str = "_orna_data";
pub(crate) const OBJECT_ID_COLUMN: &str = "_orna_object_id";

pub(crate) fn relation_name(type_id: TypeId) -> String {
    format!("t_{}", type_id_hex(type_id))
}

pub(crate) fn field_name(field_id: FieldId) -> String {
    format!("f_{}", field_id_hex(field_id))
}

pub(crate) fn unique_constraint_name(field_id: FieldId) -> String {
    format!("uq_{}", field_id_hex(field_id))
}

pub(crate) fn type_id_hex(type_id: TypeId) -> String {
    raw_id_hex(type_id.to_bytes())
}

pub(crate) fn field_id_hex(field_id: FieldId) -> String {
    raw_id_hex(field_id.to_bytes())
}

fn raw_id_hex(bytes: [u8; 16]) -> String {
    format!("{:032x}", u128::from_be_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use orna_core::{FieldId, TypeId};

    use super::{field_id_hex, field_name, relation_name, type_id_hex, unique_constraint_name};

    #[test]
    fn names_use_exact_lowercase_raw_identity_bytes() {
        let type_id = TypeId::from_bytes([
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ]);
        let field_id = FieldId::from_bytes([0xab; 16]);

        assert_eq!(relation_name(type_id), "t_000102030405060708090a0b0c0d0e0f");
        assert_eq!(field_name(field_id), "f_abababababababababababababababab");
        assert_eq!(
            unique_constraint_name(field_id),
            "uq_abababababababababababababababab"
        );
        assert!(format!("ck_{}_object_id", type_id_hex(type_id)).len() < 63);
        assert!(format!("ck_{}_object_id", field_id_hex(field_id)).len() < 63);
        assert!(unique_constraint_name(field_id).len() < 63);
    }
}
