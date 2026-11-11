//! Checks the patterns of a body: each binder and definition reference a pattern names.

use super::ValidationError;
use crate::{Body, BodyId, Pat, Program};

impl Program {
    pub(super) fn validate_pat(
        &self,
        body_id: BodyId,
        body: &Body,
        pat: &Pat,
        errors: &mut Vec<ValidationError>,
    ) {
        match pat {
            Pat::Wild | Pat::Lit(_) => {}
            Pat::Bind { local } => self.validate_local(body_id, body, *local, errors),
            Pat::Tuple(pats) => {
                for pat in pats.iter() {
                    self.validate_pat_ref(body_id, body, *pat, errors);
                }
            }
            Pat::List {
                prefix,
                rest,
                suffix,
            } => {
                for pat in prefix.iter().chain(suffix.iter()) {
                    self.validate_pat_ref(body_id, body, *pat, errors);
                }
                if let Some(rest) = rest {
                    self.validate_local(body_id, body, *rest, errors);
                }
            }
            Pat::Record(fields) => {
                for (_, pat) in fields.iter() {
                    self.validate_pat_ref(body_id, body, *pat, errors);
                }
            }
            Pat::Ctor { def, fields } => {
                self.validate_def_ref(*def, errors);
                for (field, pat) in fields.iter() {
                    self.validate_def_ref(*field, errors);
                    self.validate_pat_ref(body_id, body, *pat, errors);
                }
            }
            Pat::Some(inner) => self.validate_pat_ref(body_id, body, *inner, errors),
        }
    }
}
