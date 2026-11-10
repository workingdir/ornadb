//! Checks the generic parameters of definitions: their protocol bounds and the bounds' arguments.

use super::ValidationError;
use crate::{DefKind, GenericParam, Program};

impl Program {
    pub(super) fn validate_generics(
        &self,
        generics: &[GenericParam],
        errors: &mut Vec<ValidationError>,
    ) {
        for generic in generics {
            for bound in &generic.bounds {
                self.validate_kinds(
                    std::slice::from_ref(&bound.protocol),
                    "generic bound",
                    DefKind::is_protocol,
                    errors,
                );
                bound
                    .args
                    .iter()
                    .for_each(|ty| self.validate_ty(*ty, "generic bound argument", errors));
            }
        }
    }
}
