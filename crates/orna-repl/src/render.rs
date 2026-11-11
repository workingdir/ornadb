//! Result rendering for the driver. Scalars use the old REPL's `<value> : <Type>` shape; composite values
//! fall back to the canonical raw form, because the old REPL's full printer is private to `orna-cli` and is
//! tied to its own canonical value type.

use orna_exec::Value;

/// Renders one non-unit result as `<value> : <Type>`.
#[must_use]
pub fn result(value: &Value) -> String {
    match value {
        Value::Bool(flag) => format!("{flag} : Bool"),
        Value::Int(number) => format!("{number} : Int"),
        Value::Text(text) => format!("{text:?} : Str"),
        other => composite(other),
    }
}

fn composite(value: &Value) -> String {
    match value.to_raw() {
        Ok(raw) => format!("{raw:?} : Value"),
        Err(code) => format!("<no canonical form: {code}> : Value"),
    }
}
