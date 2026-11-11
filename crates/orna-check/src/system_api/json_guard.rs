//! Test-only oracle for the `sys` descriptor: the retired JSON decoder.
//!
//! `sys.orna` is the authority. The generated JSON artifact is still decoded here, so the guard test can prove the
//! native declarations reproduce it field by field.

use super::*;

pub(super) fn raw_document(source: &str) -> Result<RawSystemApi, SystemApiError> {
    reject_duplicate_json_members(source)?;
    let value: serde_json::Value =
        serde_json::from_str(source).map_err(|_| SystemApiError::InvalidJson)?;
    let document = object(&value)?;
    let counts = raw_counts(value_at(document, "counts")?)?;
    let singletons = values(value_at(document, "singletons")?)?
        .iter()
        .map(raw_singleton)
        .collect::<Result<_, _>>()?;
    let opaque_identifiers = strings(value_at(document, "opaque_identifiers")?)?;
    let reference_aliases = values(value_at(document, "reference_aliases")?)?
        .iter()
        .map(|value| {
            Ok(RawReferenceAlias {
                name: text(value_at(object(value)?, "name")?)?.to_owned(),
                target: text(value_at(object(value)?, "target")?)?.to_owned(),
                definition: text(value_at(object(value)?, "definition")?)?.to_owned(),
            })
        })
        .collect::<Result<_, SystemApiError>>()?;
    let value_types = values(value_at(document, "value_types")?)?
        .iter()
        .map(raw_value_type)
        .collect::<Result<_, _>>()?;
    let enums = object(value_at(document, "enums")?)?
        .iter()
        .map(|(name, variants)| Ok((name.clone(), strings(variants)?)))
        .collect::<Result<_, SystemApiError>>()?;
    let relations = values(value_at(document, "relations")?)?
        .iter()
        .map(raw_relation)
        .collect::<Result<_, _>>()?;
    let functions = values(value_at(document, "functions")?)?
        .iter()
        .map(raw_function)
        .collect::<Result<_, _>>()?;
    let failure_codes = strings(value_at(document, "failure_codes")?)?;
    let removed_names = object(value_at(document, "removed_names")?)?
        .iter()
        .map(|(name, value)| {
            let value = object(value)?;
            Ok((
                name.clone(),
                RawRemovedName {
                    replacement: match value_at(value, "replacement")? {
                        value if value.is_null() => None,
                        value => Some(text(value)?.to_owned()),
                    },
                    diagnostic: text(value_at(value, "diagnostic")?)?.to_owned(),
                },
            ))
        })
        .collect::<Result<_, SystemApiError>>()?;
    Ok(RawSystemApi {
        counts,
        singletons,
        opaque_identifiers,
        reference_aliases,
        value_types,
        enums,
        relations,
        functions,
        failure_codes,
        removed_names,
    })
}

/// `serde_json::Value` stores object members in a map and therefore cannot
/// report duplicate keys after decoding.  The API artifact is a closed
/// descriptor, so accepting a duplicate would make validation depend on which
/// occurrence the decoder retained.  Scan the JSON structure first and
/// compare decoded member names (including escaped spellings); serde_json
/// remains responsible for the complete syntax and value validation below.
fn reject_duplicate_json_members(source: &str) -> Result<(), SystemApiError> {
    let mut parser = JsonMemberParser {
        source: source.as_bytes(),
        position: 0,
        depth: 0,
    };
    parser.value()?;
    Ok(())
}

struct JsonMemberParser<'a> {
    source: &'a [u8],
    position: usize,
    depth: usize,
}

impl JsonMemberParser<'_> {
    fn value(&mut self) -> Result<(), SystemApiError> {
        self.whitespace();
        match self.source.get(self.position).copied() {
            Some(b'{') => {
                self.enter_container()?;
                let result = self.object();
                self.depth -= 1;
                result
            }
            Some(b'[') => {
                self.enter_container()?;
                let result = self.array();
                self.depth -= 1;
                result
            }
            Some(b'"') => {
                self.string()?;
                Ok(())
            }
            Some(b't') => self.literal(b"true"),
            Some(b'f') => self.literal(b"false"),
            Some(b'n') => self.literal(b"null"),
            Some(b'-' | b'0'..=b'9') => {
                self.primitive();
                Ok(())
            }
            _ => Err(SystemApiError::InvalidJson),
        }
    }

    fn enter_container(&mut self) -> Result<(), SystemApiError> {
        if self.depth >= MAX_JSON_NESTING_DEPTH {
            return Err(SystemApiError::InvalidJson);
        }
        self.depth += 1;
        Ok(())
    }

    fn object(&mut self) -> Result<(), SystemApiError> {
        self.position += 1;
        self.whitespace();
        let mut names = BTreeSet::new();
        if self.take(b'}') {
            return Ok(());
        }
        loop {
            self.whitespace();
            let name = self.string()?;
            if !names.insert(name) {
                return Err(SystemApiError::InvalidJson);
            }
            self.whitespace();
            if !self.take(b':') {
                return Err(SystemApiError::InvalidJson);
            }
            self.value()?;
            self.whitespace();
            if self.take(b'}') {
                return Ok(());
            }
            if !self.take(b',') {
                return Err(SystemApiError::InvalidJson);
            }
        }
    }

    fn array(&mut self) -> Result<(), SystemApiError> {
        self.position += 1;
        self.whitespace();
        if self.take(b']') {
            return Ok(());
        }
        loop {
            self.value()?;
            self.whitespace();
            if self.take(b']') {
                return Ok(());
            }
            if !self.take(b',') {
                return Err(SystemApiError::InvalidJson);
            }
        }
    }

    fn string(&mut self) -> Result<String, SystemApiError> {
        let start = self.position;
        if !self.take(b'"') {
            return Err(SystemApiError::InvalidJson);
        }
        let mut escaped = false;
        while let Some(byte) = self.source.get(self.position).copied() {
            self.position += 1;
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                return serde_json::from_slice(&self.source[start..self.position])
                    .map_err(|_| SystemApiError::InvalidJson);
            }
        }
        Err(SystemApiError::InvalidJson)
    }

    fn literal(&mut self, literal: &[u8]) -> Result<(), SystemApiError> {
        let end = self
            .position
            .checked_add(literal.len())
            .ok_or(SystemApiError::InvalidJson)?;
        if self.source.get(self.position..end) == Some(literal) {
            self.position = end;
            Ok(())
        } else {
            Err(SystemApiError::InvalidJson)
        }
    }

    fn primitive(&mut self) {
        while self
            .source
            .get(self.position)
            .is_some_and(|byte| !matches!(byte, b' ' | b'\n' | b'\r' | b'\t' | b',' | b']' | b'}'))
        {
            self.position += 1;
        }
    }

    fn whitespace(&mut self) {
        while self
            .source
            .get(self.position)
            .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.position += 1;
        }
    }

    fn take(&mut self, expected: u8) -> bool {
        if self.source.get(self.position) == Some(&expected) {
            self.position += 1;
            true
        } else {
            false
        }
    }
}

fn raw_counts(value: &serde_json::Value) -> Result<RawCounts, SystemApiError> {
    let value = object(value)?;
    Ok(RawCounts {
        singletons: natural(value_at(value, "singletons")?)?,
        opaque_identifiers: natural(value_at(value, "opaque_identifiers")?)?,
        reference_aliases: natural(value_at(value, "reference_aliases")?)?,
        value_types: natural(value_at(value, "value_types")?)?,
        enums: natural(value_at(value, "enums")?)?,
        relations: natural(value_at(value, "relations")?)?,
        functions: natural(value_at(value, "functions")?)?,
        failure_codes: natural(value_at(value, "failure_codes")?)?,
    })
}

fn raw_singleton(value: &serde_json::Value) -> Result<RawSingleton, SystemApiError> {
    let value = object(value)?;
    Ok(RawSingleton {
        name: text(value_at(value, "name")?)?.to_owned(),
        ty: text(value_at(value, "type")?)?.to_owned(),
        availability: text(value_at(value, "availability")?)?.to_owned(),
    })
}

fn raw_value_type(value: &serde_json::Value) -> Result<RawValueType, SystemApiError> {
    let value = object(value)?;
    Ok(RawValueType {
        name: text(value_at(value, "name")?)?.to_owned(),
        kind: text(value_at(value, "kind")?)?.to_owned(),
        purpose: text(value_at(value, "purpose")?)?.to_owned(),
        type_parameters: optional_strings(value.get("type_parameters"))?,
        fields: optional_values(value.get("fields"))?
            .iter()
            .map(raw_field)
            .collect::<Result<_, _>>()?,
        invariants: strings(value_at(value, "invariants")?)?,
    })
}

fn raw_relation(value: &serde_json::Value) -> Result<RawRelation, SystemApiError> {
    let value = object(value)?;
    Ok(RawRelation {
        name: text(value_at(value, "name")?)?.to_owned(),
        grouped_handle: text(value_at(value, "grouped_handle")?)?.to_owned(),
        kind: text(value_at(value, "kind")?)?.to_owned(),
        availability: text(value_at(value, "availability")?)?.to_owned(),
        key: text(value_at(value, "key")?)?.to_owned(),
        purpose: text(value_at(value, "purpose")?)?.to_owned(),
        key_fields: strings(value_at(value, "key_fields")?)?,
        invariants: optional_strings(value.get("invariants"))?,
        fields: values(value_at(value, "fields")?)?
            .iter()
            .map(raw_field)
            .collect::<Result<_, _>>()?,
        writable: boolean(value_at(value, "writable")?)?,
        reference_type: text(value_at(value, "reference_type")?)?.to_owned(),
    })
}

fn raw_field(value: &serde_json::Value) -> Result<RawField, SystemApiError> {
    let value = object(value)?;
    Ok(RawField {
        name: text(value_at(value, "name")?)?.to_owned(),
        ty: text(value_at(value, "type")?)?.to_owned(),
    })
}

fn raw_function(value: &serde_json::Value) -> Result<RawFunction, SystemApiError> {
    let value = object(value)?;
    Ok(RawFunction {
        name: text(value_at(value, "name")?)?.to_owned(),
        signature: text(value_at(value, "signature")?)?.to_owned(),
        effect: text(value_at(value, "effect")?)?.to_owned(),
        purpose: text(value_at(value, "purpose")?)?.to_owned(),
        since: optional_text(value.get("since"))?,
        contract: optional_text(value.get("contract"))?,
        preconditions: optional_text(value.get("preconditions"))?,
        ownership: optional_text(value.get("ownership"))?,
        snapshot_rule: optional_text(value.get("snapshot_rule"))?,
    })
}

fn object(
    value: &serde_json::Value,
) -> Result<&serde_json::Map<String, serde_json::Value>, SystemApiError> {
    value.as_object().ok_or(SystemApiError::InvalidJson)
}

fn values(value: &serde_json::Value) -> Result<&Vec<serde_json::Value>, SystemApiError> {
    value.as_array().ok_or(SystemApiError::InvalidJson)
}

fn text(value: &serde_json::Value) -> Result<&str, SystemApiError> {
    value.as_str().ok_or(SystemApiError::InvalidJson)
}

fn natural(value: &serde_json::Value) -> Result<usize, SystemApiError> {
    value
        .as_u64()
        .and_then(|value| value.try_into().ok())
        .ok_or(SystemApiError::InvalidJson)
}

fn boolean(value: &serde_json::Value) -> Result<bool, SystemApiError> {
    value.as_bool().ok_or(SystemApiError::InvalidJson)
}

fn value_at<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<&'a serde_json::Value, SystemApiError> {
    object.get(key).ok_or(SystemApiError::InvalidJson)
}

fn strings(value: &serde_json::Value) -> Result<Vec<String>, SystemApiError> {
    values(value)?
        .iter()
        .map(|value| Ok(text(value)?.to_owned()))
        .collect()
}

fn optional_values(
    value: Option<&serde_json::Value>,
) -> Result<&[serde_json::Value], SystemApiError> {
    match value {
        Some(value) => Ok(values(value)?.as_slice()),
        None => Ok(&[]),
    }
}

fn optional_strings(value: Option<&serde_json::Value>) -> Result<Vec<String>, SystemApiError> {
    match value {
        Some(value) => strings(value),
        None => Ok(Vec::new()),
    }
}

fn optional_text(value: Option<&serde_json::Value>) -> Result<Option<String>, SystemApiError> {
    value
        .map(text)
        .transpose()
        .map(|value| value.map(str::to_owned))
}

/// Asserts two descriptor sequences agree row by row, naming the first row that differs.
fn assert_rows_match<T: PartialEq + std::fmt::Debug>(what: &str, native: &[T], json: &[T]) {
    for (index, (native_row, json_row)) in native.iter().zip(json).enumerate() {
        assert_eq!(native_row, json_row, "{what} row {index} differs");
    }
    assert_eq!(native.len(), json.len(), "{what} row count differs");
}

#[test]
fn native_declarations_reproduce_the_json_descriptor_field_by_field() {
    let json = raw_document(include_str!(
        "../../../orna-sys-id/catalogue/sys-1.1.0.json"
    ))
    .expect("the generated JSON descriptor decodes");
    let native = super::native_raw::native_raw_document().expect("sys.orna decodes");
    assert_eq!(native.counts, json.counts);
    assert_rows_match("singletons", &native.singletons, &json.singletons);
    assert_rows_match(
        "opaque identifiers",
        &native.opaque_identifiers,
        &json.opaque_identifiers,
    );
    assert_rows_match(
        "reference aliases",
        &native.reference_aliases,
        &json.reference_aliases,
    );
    assert_rows_match("value types", &native.value_types, &json.value_types);
    assert_eq!(native.enums, json.enums);
    assert_rows_match("relations", &native.relations, &json.relations);
    assert_rows_match("functions", &native.functions, &json.functions);
    assert_eq!(native.failure_codes, json.failure_codes);
    assert_eq!(native.removed_names, json.removed_names);
}

#[test]
fn native_relations_are_read_only() {
    let native = super::native_raw::native_raw_document().expect("sys.orna decodes");
    assert!(!native.relations.is_empty());
    for relation in &native.relations {
        assert!(
            !relation.writable,
            "relation `{}` must be read-only",
            relation.name
        );
    }
}
