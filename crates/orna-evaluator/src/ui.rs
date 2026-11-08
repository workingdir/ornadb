//! Evaluator-side support for pinned `std.ui` presentation constructors.
//!
//! Nodes and action descriptors stay renderer-neutral data; an admitted runtime
//! owns interpreting actions.

use super::*;

impl Context<'_, '_> {
    pub(super) fn ui_action(
        &mut self,
        arguments: &[orna_syntax::Argument],
        scope: &mut Scope,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(arguments.len())?;
        let mut action_id = None;
        let mut input_type = None;
        let mut debug_kind = None;
        let mut positional = 0usize;
        for argument in arguments {
            match argument.name.as_deref() {
                Some("action_id") if action_id.is_none() => {
                    let value = self.evaluate(&argument.value, scope, depth + 1)?;
                    let Value::String(value) = value else {
                        return Err(error("ORNA-EVAL-TYPE"));
                    };
                    action_id = Some(value);
                }
                Some("as") if input_type.is_none() => {
                    let Some(name) = function_name(&argument.value) else {
                        return Err(error("ORNA-EVAL-ARGUMENT"));
                    };
                    input_type = ui_input_type_name(&name).map(str::to_owned);
                }
                Some("debug_kind") if debug_kind.is_none() => {
                    let value = self.evaluate(&argument.value, scope, depth + 1)?;
                    match value {
                        Value::String(value) => debug_kind = Some(value),
                        Value::Null => debug_kind = Some(String::new()),
                        _ => return Err(error("ORNA-EVAL-TYPE")),
                    }
                }
                None if positional == 0 && action_id.is_none() => {
                    let value = self.evaluate(&argument.value, scope, depth + 1)?;
                    let Value::String(value) = value else {
                        return Err(error("ORNA-EVAL-TYPE"));
                    };
                    action_id = Some(value);
                    positional += 1;
                }
                _ => return Err(error("ORNA-EVAL-ARGUMENT")),
            }
        }
        let action_id = action_id
            .filter(|value| !value.is_empty())
            .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
        let input_type = input_type.ok_or_else(|| error("ORNA-EVAL-ARGUMENT"))?;
        self.string(action_id.clone())?;
        self.string(input_type.clone())?;
        let mut fields = BTreeMap::new();
        fields.insert("action_id".into(), Value::String(action_id));
        fields.insert("input_type".into(), Value::String(input_type));
        fields.insert(
            "debug_kind".into(),
            match debug_kind {
                Some(value) if !value.is_empty() => Value::String(value),
                _ => Value::Null,
            },
        );
        Ok(Value::Record(fields))
    }

    pub(super) fn ui_node(&mut self, values: Vec<Value>) -> Result<Value, EvaluationError> {
        let [
            Value::String(kind),
            Value::Record(properties),
            Value::List(children),
            Value::List(actions),
            Value::List(property_types),
        ] = values.as_slice()
        else {
            return Err(error("ORNA-EVAL-TYPE"));
        };
        let contract_name = match kind.as_str() {
            "field" | "text" | "rows" | "cols" | "stack" | "details" | "table" | "tree"
            | "code" | "diff" | "chart" | "button" | "form" | "input" | "image" | "audio"
            | "download" => kind,
            _ => return Err(error("ORNA-EVAL-ARGUMENT")),
        };
        self.items(properties.len() + children.len() + actions.len() + property_types.len() + 5)?;
        self.string(contract_name.clone())?;

        let mut type_overrides = BTreeMap::new();
        for hint in property_types {
            let Value::Tuple(pair) = hint else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            let [Value::String(name), Value::String(type_name)] = pair.as_slice() else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            if name.is_empty()
                || type_name.is_empty()
                || !properties.contains_key(name)
                || type_overrides
                    .insert(name.clone(), type_name.clone())
                    .is_some()
            {
                return Err(error("ORNA-EVAL-ARGUMENT"));
            }
            self.string(name.clone())?;
            self.string(type_name.clone())?;
        }

        let mut action_map = BTreeMap::new();
        for action in actions {
            let Value::Tuple(pair) = action else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            let [Value::String(name), descriptor] = pair.as_slice() else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            if name.is_empty() || !is_ui_action_descriptor(descriptor) {
                return Err(error("ORNA-EVAL-ARGUMENT"));
            }
            self.string(name.clone())?;
            if action_map
                .insert(name.clone(), descriptor.clone())
                .is_some()
            {
                return Err(error("ORNA-EVAL-ARGUMENT"));
            }
        }

        if kind == "input" {
            let Value::Record(descriptor) = action_map
                .get("change")
                .ok_or_else(|| error("ORNA-EVAL-ARGUMENT"))?
            else {
                return Err(error("ORNA-EVAL-ARGUMENT"));
            };
            let Some(Value::String(input_type)) = descriptor.get("input_type") else {
                return Err(error("ORNA-EVAL-ARGUMENT"));
            };
            match properties.get("value") {
                Some(Value::Option(Some(value)))
                    if ui_value_type_name(value) != input_type.as_str() =>
                {
                    return Err(error("ORNA-EVAL-ARGUMENT"));
                }
                Some(Value::Option(None) | Value::Null) => {
                    type_overrides
                        .entry("value".into())
                        .or_insert_with(|| format!("std.option<{input_type}>"));
                }
                Some(Value::Option(Some(_))) => {}
                _ => return Err(error("ORNA-EVAL-TYPE")),
            }
        }

        // A relation-valued property (a table node's rows) is materialised to a
        // list of records first: `canonical()` refuses a relation, and a node
        // must stay renderer-neutral data (ORNA-APP-003).
        let mut materialized = Vec::with_capacity(properties.len());
        for (name, value) in properties {
            let value = match value {
                Value::Relation(plan) => {
                    let mut rows = Vec::new();
                    self.for_each_relation_value(plan, 1, |context, row| {
                        rows.push(row);
                        context.items(rows.len())?;
                        Ok(true)
                    })?;
                    Value::list(rows)
                }
                other => other.clone(),
            };
            materialized.push((name.clone(), value));
        }
        let properties = materialized
            .iter()
            .map(|(name, value)| {
                self.string(name.clone())?;
                ui_property_is_canonical(value)?;
                let type_name = type_overrides
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| ui_value_type_name(value));
                if !ui_property_type_matches(&type_name, value) {
                    return Err(error("ORNA-EVAL-TYPE"));
                }
                let mut typed = BTreeMap::new();
                typed.insert("type".into(), Value::String(type_name));
                typed.insert("value".into(), value.clone());
                Ok((name.clone(), Value::Record(typed)))
            })
            .collect::<Result<BTreeMap<_, _>, EvaluationError>>()?;

        let mut slots = BTreeMap::new();
        for child in children {
            if !is_ui_presentation_node(child, 1, self.limits.max_depth)? {
                return Err(error("ORNA-EVAL-TYPE"));
            }
        }
        slots.insert("content".into(), Value::List(children.clone()));

        let mut contract = BTreeMap::new();
        contract.insert(
            "id".into(),
            Value::String(format!("std.ui.{contract_name}@1")),
        );
        contract.insert(
            "name".into(),
            Value::String(format!("std.ui.{contract_name}")),
        );
        contract.insert("version".into(), Value::String("1.0".into()));

        let mut node = BTreeMap::new();
        node.insert("kind".into(), Value::String("node".into()));
        node.insert("contract".into(), Value::Record(contract));
        node.insert("properties".into(), Value::Record(properties));
        node.insert("slots".into(), Value::Record(slots));
        node.insert("actions".into(), Value::Record(action_map));
        Ok(Value::Record(node))
    }
}

fn ui_input_type_name(name: &str) -> Option<&'static str> {
    Some(match name {
        "Text" | "Str" | "String" | "std.text" => "std.text",
        "Bool" | "BOOLEAN" | "BOOL" | "std.boolean" => "std.boolean",
        "Int" | "INTEGER" | "BIGINT" | "std.integer" => "std.integer",
        "Float" | "std.float" => "std.float",
        "Decimal" | "std.decimal" => "std.decimal",
        _ => return None,
    })
}

fn ui_value_type_name(value: &Value) -> String {
    match value {
        Value::Null => "std.null".into(),
        Value::Unit => "std.void".into(),
        Value::Bool(_) => "std.boolean".into(),
        Value::Int(_) => "std.integer".into(),
        Value::Decimal(_) => "std.decimal".into(),
        Value::Money { .. } => "std.money".into(),
        Value::Float(_) => "std.float".into(),
        Value::String(_) => "std.text".into(),
        Value::Blob(_) => "std.binary_large_object".into(),
        Value::AnnotatedBlob(_) => "std.binary_large_object".into(),
        Value::ContextualBlob(_) => "std.binary_large_object".into(),
        Value::Date(_) => "std.date".into(),
        Value::Uuid(_) => "std.uuid".into(),
        Value::Digest(_) => "std.digest".into(),
        Value::Reference(_) => "std.reference".into(),
        Value::TableReference(_) => "std.table_reference".into(),
        Value::Instant { .. } => "std.timestamp".into(),
        Value::Duration { .. } => "std.duration".into(),
        Value::Period { .. } => "std.period".into(),
        Value::Error(_) => "std.error".into(),
        Value::Range { .. } => "std.range".into(),
        Value::List(_) => "std.list".into(),
        Value::Stream { .. } => "std.stream".into(),
        Value::Relation(_) => "std.relation".into(),
        Value::Tuple(_) => "std.tuple".into(),
        Value::Record(_) => "std.record".into(),
        Value::NominalRecord { .. } => "std.nominal_record".into(),
        Value::Enum { .. } => "std.enum".into(),
        Value::Option(Some(value)) => format!("std.option<{}>", ui_value_type_name(value)),
        Value::Option(None) => "std.option<unknown>".into(),
        Value::Function { .. } | Value::Closure(_) => "std.function".into(),
    }
}

pub(super) fn ui_property_type_matches(type_name: &str, value: &Value) -> bool {
    let is_typed_option = || {
        type_name
            .strip_prefix("std.option<")
            .and_then(|inner| inner.strip_suffix('>'))
            .is_some_and(ui_type_name_is_well_formed)
    };
    match value {
        Value::Null => type_name == "std.null" || is_typed_option(),
        Value::Option(None) => is_typed_option(),
        _ => type_name == ui_value_type_name(value),
    }
}

/// Checks that a node property is canonical data. The canonical encoder refuses
/// a Blob reference, so the property is checked through its skeleton with every
/// well-formed ROV-3 reference replaced by null. Any other Blob form is refused
/// here, as it was before a UI property could carry a Blob at all.
fn ui_property_is_canonical(value: &Value) -> Result<(), EvaluationError> {
    let raw = value.raw()?;
    if !ui_blob_forms_are_references(&raw) {
        return Err(error("ORNA-EVAL-VALUE"));
    }
    let mut blob_positions = Vec::new();
    let mut next_index = 0;
    let skeleton = identity_skeleton(raw, &mut next_index, &mut blob_positions);
    CanonicalValue::new(skeleton)
        .map(drop)
        .map_err(|_| error("ORNA-EVAL-VALUE"))
}

/// Whether every Blob node in `raw` is a well-formed ROV-3 reference. Inline
/// OVB-2, SOV-3 and overflow nodes, and malformed ROV-3 references, are not.
fn ui_blob_forms_are_references(raw: &Raw) -> bool {
    match raw {
        Raw::Tag(ROV3_BLOB_TAG, _) => orna_value::blob_metadata_from_raw(raw).is_ok(),
        Raw::Tag(OVB2_BLOB_TAG | SOV3_BLOB_TAG | ROV3_OVERFLOW_TAG, _) => false,
        Raw::Tag(_, payload) => ui_blob_forms_are_references(payload),
        Raw::Array(items) => items.iter().all(ui_blob_forms_are_references),
        Raw::Map(entries) => entries.iter().all(|(key, value)| {
            ui_blob_forms_are_references(key) && ui_blob_forms_are_references(value)
        }),
        _ => true,
    }
}

fn ui_type_name_is_well_formed(type_name: &str) -> bool {
    if let Some(inner) = type_name
        .strip_prefix("std.option<")
        .and_then(|inner| inner.strip_suffix('>'))
    {
        return ui_type_name_is_well_formed(inner);
    }
    !type_name.is_empty()
        && type_name.split('.').all(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .is_some_and(|first| first == '_' || first.is_alphabetic())
                && chars.all(|char| char == '_' || char.is_alphanumeric())
        })
}

pub(super) fn is_ui_presentation_node(
    value: &Value,
    depth: usize,
    max_depth: usize,
) -> Result<bool, EvaluationError> {
    if depth > max_depth {
        return Err(error("ORNA-EVAL-LIMIT"));
    }
    let Value::Record(fields) = value else {
        return Ok(false);
    };
    if !matches!(fields.get("kind"), Some(Value::String(kind)) if kind == "node") {
        return Ok(false);
    }
    let Some(Value::Record(contract)) = fields.get("contract") else {
        return Ok(false);
    };
    if !["id", "name", "version"]
        .into_iter()
        .all(|name| matches!(contract.get(name), Some(Value::String(value)) if !value.is_empty()))
    {
        return Ok(false);
    }
    let Some(Value::Record(properties)) = fields.get("properties") else {
        return Ok(false);
    };
    if !properties.iter().all(|(name, property)| {
        let Value::Record(typed) = property else {
            return false;
        };
        let (Some(Value::String(type_name)), Some(value)) = (typed.get("type"), typed.get("value"))
        else {
            return false;
        };
        !name.is_empty()
            && ui_property_type_matches(type_name, value)
            && ui_property_is_canonical(value).is_ok()
    }) {
        return Ok(false);
    }
    let Some(Value::Record(slots)) = fields.get("slots") else {
        return Ok(false);
    };
    let child_depth = depth
        .checked_add(1)
        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
    for slot in slots.values() {
        let Value::List(children) = slot else {
            return Ok(false);
        };
        for child in children {
            if !is_ui_presentation_node(child, child_depth, max_depth)? {
                return Ok(false);
            }
        }
    }
    let Some(Value::Record(actions)) = fields.get("actions") else {
        return Ok(false);
    };
    Ok(actions.values().all(is_ui_action_descriptor))
}

fn is_ui_action_descriptor(value: &Value) -> bool {
    let Value::Record(fields) = value else {
        return false;
    };
    let has_text =
        |name: &str| matches!(fields.get(name), Some(Value::String(value)) if !value.is_empty());
    has_text("action_id")
        && has_text("input_type")
        && fields
            .get("debug_kind")
            .is_none_or(|value| matches!(value, Value::Null | Value::String(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_blob_non_canonical_property_is_still_rejected() {
        let value = Value::Float(f64::NAN.to_bits() ^ 1);
        assert!(ui_property_is_canonical(&value).is_err());
    }

    #[test]
    fn plain_property_is_accepted() {
        assert!(ui_property_is_canonical(&Value::String("label".into())).is_ok());
    }

    #[test]
    fn inline_blob_forms_are_not_admitted_in_a_property() {
        let inline = Raw::Tag(OVB2_BLOB_TAG, Box::new(Raw::Null));
        let sov3 = Raw::Tag(SOV3_BLOB_TAG, Box::new(Raw::Null));
        let overflow = Raw::Tag(ROV3_OVERFLOW_TAG, Box::new(Raw::Null));
        assert!(!ui_blob_forms_are_references(&inline));
        assert!(!ui_blob_forms_are_references(&sov3));
        assert!(!ui_blob_forms_are_references(&overflow));
        let nested = Raw::Array(vec![Raw::Map(vec![(Raw::Null, sov3)])]);
        assert!(!ui_blob_forms_are_references(&nested));
    }

    #[test]
    fn malformed_rov3_reference_is_not_admitted_in_a_property() {
        let malformed = Raw::Tag(ROV3_BLOB_TAG, Box::new(Raw::Array(Vec::new())));
        assert!(!ui_blob_forms_are_references(&malformed));
    }
}
