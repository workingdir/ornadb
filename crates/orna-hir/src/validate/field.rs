//! Checks a field definition: its type and, for a table field, the body of its initializer.

use super::ValidationError;
use crate::{DefId, FieldDef, Program};

impl Program {
    pub(super) fn validate_field(
        &self,
        field: &FieldDef,
        from: DefId,
        errors: &mut Vec<ValidationError>,
    ) {
        self.validate_ty(field.ty, "field.ty", errors);
        if let Some(initializer) = field.initializer {
            self.validate_owned_body(initializer.body(), from, "field initializer", errors);
        }
    }
}
