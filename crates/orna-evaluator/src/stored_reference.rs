//! Reading through a stored table reference (ORNA-REF-001, ORNA-REF-002).
//!
//! A stored reference (tag 60021) holds the database identity, the table
//! ObjectId and the primary key of one row, and embeds no copy of it
//! (ORNA-REF-001). `reference.key` is therefore available without loading the
//! target row, and every other field resolves the row through the activation's
//! own snapshot, so it answers from the same state as every other read
//! (ORNA-REF-002).

use orna_value::{CanonicalValue, Raw};

use super::{Context, EvaluationError, Scope, Value, error};

/// The diagnostic code of a reference whose target row is not live.
const DANGLING_REFERENCE: &str = "ORNA-REF-DANGLING";

impl Context<'_, '_> {
    /// Reads `name` through the stored table reference `reference`.
    pub(super) fn table_reference_field(
        &mut self,
        reference: &CanonicalValue,
        name: &str,
        scope: &Scope,
    ) -> Result<Value, EvaluationError> {
        let Raw::Tag(60021, payload) = reference.raw() else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        let Raw::Array(identity) = payload.as_ref() else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        let [_, _, key] = identity.as_slice() else {
            return Err(error("ORNA-EVAL-VALUE"));
        };

        // The primary key is part of the reference itself, so reading it does
        // not fetch or materialize the referenced row.
        if name == "key" {
            let key = CanonicalValue::new(key.clone()).map_err(|_| error("ORNA-EVAL-VALUE"))?;
            return Value::from_canonical(&key, self, 1);
        }

        self.invalidate_call_reuse();
        // A Blob-holding target is served as a context value, keeping its
        // profile; any other target resolves through the canonical snapshot.
        let context_row = self
            .effects
            .as_deref_mut()
            .ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?
            .resolve_context_table_reference(reference)?;
        let target = match context_row {
            Some(row) => Value::from_context_value(&row, self, 0)?,
            None => {
                let canonical = self
                    .with_effect_budget(|context, budget| {
                        context
                            .effects
                            .as_deref_mut()
                            .ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?
                            .resolve_table_reference_with_budget(reference, budget)
                    })?
                    .ok_or_else(|| error(DANGLING_REFERENCE))?;
                Value::from_canonical(&canonical, self, 0)?
            }
        };
        match target {
            Value::Record(fields) => fields
                .get(name)
                .cloned()
                .ok_or_else(|| error("ORNA-EVAL-FIELD")),
            Value::NominalRecord { type_id, fields } => {
                self.nominal_field(&type_id, &fields, name, scope)
            }
            _ => Err(error("ORNA-EVAL-TYPE")),
        }
    }
}
