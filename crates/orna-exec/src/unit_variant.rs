//! A payload-free source enum variant used as a value, such as `Ordering.less`.

use orna_hir::{DefId, DefKind};

use crate::error::Stop;
use crate::machine::Machine;
use crate::value::Value;

impl Machine<'_> {
    /// Whether `def` is a source enum variant with no payload fields, so its value is the variant alone.
    pub(crate) fn is_unit_variant(&self, def: DefId) -> bool {
        matches!(&self.program.defs[def].kind, DefKind::Variant(variant) if variant.fields.is_empty())
            && self.variant_ids_of(def).is_some()
    }

    /// The value of a payload-free source enum variant.
    pub(crate) fn unit_variant(&self, def: DefId) -> Result<Value, Stop> {
        let ids = self
            .variant_ids_of(def)
            .ok_or(Stop::NotYet("money or type constructor (H4)"))?;
        Ok(Value::Enum {
            type_id: ids.type_id,
            variant_id: ids.variant_id,
            payload: None,
        })
    }
}
