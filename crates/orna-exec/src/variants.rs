//! The ObjectIds of every source enum variant of a program.
//!
//! The executor derives each variant's identity from the same qualified name
//! the old pipeline uses, so a value built here compares equal to one built by
//! the old evaluator.

use std::collections::HashMap;

use orna_hir::{DefId, DefKind, Program};
use orna_value::{enum_type_id, enum_variant_id, qualified_name};

/// The ids a source enum variant carries at run time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VariantIds {
    /// The declaring enum's ObjectId.
    pub(crate) type_id: [u8; 16],
    /// The variant's ObjectId.
    pub(crate) variant_id: [u8; 16],
}

/// Every source enum variant of `program`, keyed by its variant definition.
pub(crate) fn variant_ids(program: &Program) -> HashMap<DefId, VariantIds> {
    let mut ids = HashMap::new();
    for definition in program.defs.iter() {
        let DefKind::Enum(enum_def) = &definition.kind else {
            continue;
        };
        let namespace = program.modules[definition.module].path.to_string();
        let qualified = qualified_name(Some(&namespace), definition.name.as_str());
        let type_id = enum_type_id(&qualified);
        for &variant in &enum_def.variants {
            let variant_name = program.defs[variant].name.as_str();
            let variant_id = enum_variant_id(&qualified, variant_name);
            ids.insert(
                variant,
                VariantIds {
                    type_id,
                    variant_id,
                },
            );
        }
    }
    ids
}
