//! Standard-library dispatch for the text family.
//!
//! Pure motion from `lib.rs` (stage S6): the dispatch functions and their
//! helpers keep their bodies; only the module boundary changed.

use crate::*;
use crate::{Context, Value, error};

impl Context<'_, '_> {
    pub(crate) fn text(
        &mut self,
        name: &str,
        values: Vec<Value>,
    ) -> Result<Value, EvaluationError> {
        let name = name.strip_prefix("__").unwrap_or(name);
        match (name, values.as_slice()) {
            ("trim", [Value::String(value)]) => {
                // Keep the pinned standard profile independent of the Rust
                // toolchain's Unicode tables. Unicode 16.0.0's White_Space
                // property is stable and explicitly listed above.
                self.string(value.trim_matches(unicode_16_white_space).to_owned())
                    .map(Value::String)
            }
            ("split", [Value::String(value), Value::String(separator)]) => {
                // This profile preserves leading/trailing empty fields. An
                // empty separator iterates Unicode scalar values, as required
                // by the portable std.text contract.
                let count = if separator.is_empty() {
                    value.chars().count()
                } else {
                    value.split(separator).count()
                };
                self.items(count)?;
                let mut fields = Vec::with_capacity(count);
                if separator.is_empty() {
                    for scalar in value.chars() {
                        self.step()?;
                        fields.push(Value::String(scalar.to_string()));
                    }
                } else {
                    for field in value.split(separator) {
                        self.step()?;
                        fields.push(Value::String(field.to_owned()));
                    }
                }
                Ok(Value::list(fields))
            }
            ("join", [Value::List(values), Value::String(separator)]) => {
                self.items(values.len())?;
                let mut output = String::new();
                for (index, value) in values.iter().enumerate() {
                    let Value::String(value) = value else {
                        return Err(error("ORNA-EVAL-TYPE"));
                    };
                    self.step()?;
                    if index > 0 {
                        output.push_str(separator);
                    }
                    output.push_str(value);
                    if output.len() > self.limits.max_string_bytes {
                        return Err(error("ORNA-EVAL-LIMIT"));
                    }
                }
                Ok(Value::String(output))
            }
            ("starts_with", [Value::String(value), Value::String(prefix)]) => {
                Ok(Value::Bool(value.starts_with(prefix)))
            }
            ("ends_with", [Value::String(value), Value::String(suffix)]) => {
                Ok(Value::Bool(value.ends_with(suffix)))
            }
            ("contains", [Value::String(value), Value::String(needle)]) => {
                Ok(Value::Bool(value.contains(needle)))
            }
            ("replace", [Value::String(value), Value::String(from), Value::String(to)]) => {
                // `str::replace` is non-overlapping and left-to-right.
                self.string(value.replace(from, to)).map(Value::String)
            }
            ("normalise", [Value::String(value), Value::String(form)]) => match form.as_str() {
                // The reference leaves the form labels open; expose the two
                // canonical forms directly and fail closed for other labels.
                "NFC" => self.string(value.nfc().collect()).map(Value::String),
                "NFD" => self.string(value.nfd().collect()).map(Value::String),
                _ => Err(error("ORNA-EVAL-VALUE")),
            },
            ("lower", [Value::String(value)]) => {
                self.string(unicode_16_lowercase(value)).map(Value::String)
            }
            ("upper", [Value::String(value)]) => {
                self.string(unicode_16_uppercase(value)).map(Value::String)
            }
            ("trim" | "lower" | "upper", [_])
            | ("split" | "starts_with" | "ends_with" | "contains", [_, _])
            | ("join", [_, _])
            | ("replace", [_, _, _])
            | ("normalise", [_, _]) => Err(error("ORNA-EVAL-TYPE")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
}
