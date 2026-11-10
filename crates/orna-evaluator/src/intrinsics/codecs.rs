//! Standard-library dispatch for the codecs family.
//!
//! Pure motion from `lib.rs` (stage S6): the dispatch functions and their
//! helpers keep their bodies; only the module boundary changed.

use crate::*;
use crate::{Context, Value, error};

impl Context<'_, '_> {
    pub(crate) fn base64(
        &mut self,
        name: &str,
        values: Vec<Value>,
    ) -> Result<Value, EvaluationError> {
        match (name, values.as_slice()) {
            ("__encode" | "encode", [Value::Blob(bytes)]) => {
                self.items(bytes.len())?;
                self.string(orna_value::base64_encode(bytes))
                    .map(Value::String)
            }
            ("__decode" | "decode", [Value::String(text)]) => {
                self.string(text.clone())?;
                let bytes = decode_base64(text)?;
                self.items(bytes.len())?;
                Ok(Value::Blob(bytes))
            }
            ("__encode" | "encode" | "__decode" | "decode", _) => Err(error("ORNA-EVAL-TYPE")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    pub(crate) fn random(
        &mut self,
        name: &str,
        values: Vec<Value>,
        callee: &Expr,
    ) -> Result<Value, EvaluationError> {
        self.step()?;
        match (name, values.as_slice()) {
            ("bytes" | "__bytes", [Value::Int(count)]) => {
                let count = count.to_usize().ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                self.items(count)?;
                self.random_entropy(callee, count).map(Value::Blob)
            }
            ("integer" | "__integer", [Value::Int(lower), Value::Int(upper)]) => {
                if upper <= lower {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                let width = upper - lower;
                let width = width.to_biguint().ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                let bit_count = width.bits();
                let byte_count =
                    usize::try_from(bit_count.div_ceil(8)).map_err(|_| error("ORNA-EVAL-LIMIT"))?;
                self.items(byte_count)?;
                let high_byte_bits = (bit_count % 8) as u8;
                loop {
                    self.step()?;
                    let mut sample = self.random_entropy(callee, byte_count)?;
                    if high_byte_bits != 0 {
                        let mask = (1u8 << high_byte_bits) - 1;
                        *sample.last_mut().ok_or_else(|| error("ORNA-EVAL-VALUE"))? &= mask;
                    }
                    let candidate = BigInt::from_bytes_le(Sign::Plus, &sample);
                    if candidate < BigInt::from(width.clone()) {
                        return Ok(Value::Int(lower + candidate));
                    }
                }
            }
            ("choose" | "__choose", [Value::List(values)]) => {
                if values.is_empty() {
                    return Ok(Value::Option(None));
                }
                self.items(values.len())?;
                let index = match self.random(
                    "integer",
                    vec![Value::Int(0.into()), Value::Int(BigInt::from(values.len()))],
                    callee,
                )? {
                    Value::Int(index) => {
                        index.to_usize().ok_or_else(|| error("ORNA-EVAL-VALUE"))?
                    }
                    _ => return Err(error("ORNA-EVAL-VALUE")),
                };
                let selected = values
                    .get(index)
                    .cloned()
                    .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                Ok(Value::Option(Some(Box::new(selected))))
            }
            ("shuffle" | "__shuffle", [Value::List(values)]) => {
                self.items(values.len())?;
                let mut shuffled = values.to_vec();
                for index in (1..shuffled.len()).rev() {
                    self.step()?;
                    let selected = match self.random(
                        "integer",
                        vec![Value::Int(0.into()), Value::Int(BigInt::from(index + 1))],
                        callee,
                    )? {
                        Value::Int(selected) => selected
                            .to_usize()
                            .ok_or_else(|| error("ORNA-EVAL-VALUE"))?,
                        _ => return Err(error("ORNA-EVAL-VALUE")),
                    };
                    shuffled.swap(index, selected);
                }
                Ok(Value::list(shuffled))
            }
            (
                "bytes" | "__bytes" | "integer" | "__integer" | "choose" | "__choose" | "shuffle"
                | "__shuffle",
                _,
            ) => Err(error("ORNA-EVAL-TYPE")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn random_entropy(&mut self, callee: &Expr, count: usize) -> Result<Vec<u8>, EvaluationError> {
        self.invalidate_call_reuse();
        let cancellation = self.cancellation;
        let value_raw = if self.has_ovb2_effects() {
            let arguments = [self.effect_context_value(Value::Int(BigInt::from(count)))?];
            let value = self
                .with_effect_budget(|context, budget| {
                    context
                        .ovb2_effects()
                        .ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?
                        .handle_registered_with_cancellation_and_budget(
                            "std.random.entropy",
                            callee,
                            &arguments,
                            budget,
                            cancellation,
                        )
                })?
                .ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?;
            if value.format() != ValueFormat::Ovb2 {
                return Err(error("ORNA-EVAL-VALUE"));
            }
            value.raw().clone()
        } else {
            let count_value = CanonicalValue::new(Raw::Int(BigInt::from(count)))
                .map_err(|_| error("ORNA-EVAL-VALUE"))?;
            let arguments = [count_value];
            self.with_effect_budget(|context, budget| {
                context
                    .effects
                    .as_deref_mut()
                    .ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?
                    .handle_registered_with_cancellation_and_budget(
                        "std.random.entropy",
                        callee,
                        &arguments,
                        budget,
                        cancellation,
                    )
            })?
            .ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?
            .raw()
            .clone()
        };
        let Raw::Bytes(bytes) = value_raw else {
            return Err(error("ORNA-EVAL-TYPE"));
        };
        if bytes.len() != count {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        self.items(bytes.len())?;
        Ok(bytes)
    }
    pub(crate) fn hash(
        &mut self,
        name: &str,
        values: Vec<Value>,
    ) -> Result<Value, EvaluationError> {
        self.step()?;
        match (name, values.as_slice()) {
            ("sha256", [Value::Blob(input)]) => {
                if input.len() > self.limits.max_string_bytes {
                    return Err(error("ORNA-EVAL-LIMIT"));
                }
                Ok(Value::Blob(Sha256::digest(input).to_vec()))
            }
            ("sha256_text", [Value::String(input)]) => {
                Ok(Value::Blob(Sha256::digest(input.as_bytes()).to_vec()))
            }
            ("domain_sha256", [Value::String(domain), Value::Blob(payload)]) => {
                if domain.is_empty()
                    || !domain.is_ascii()
                    || domain.as_bytes().contains(&0)
                    || payload.len() > self.limits.max_string_bytes
                {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                let mut digest = Sha256::new();
                digest.update(domain.as_bytes());
                digest.update([0]);
                digest.update(payload);
                Ok(Value::Blob(digest.finalize().to_vec()))
            }
            ("to_hex", [Value::Blob(digest)]) if digest.len() == 32 => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                let mut output = String::with_capacity(64);
                for byte in digest {
                    output.push(char::from(HEX[usize::from(byte >> 4)]));
                    output.push(char::from(HEX[usize::from(byte & 0x0f)]));
                }
                self.string(output).map(Value::String)
            }
            ("from_hex", [Value::String(encoded)]) => {
                let decoded = if encoded.len() == 64
                    && encoded
                        .as_bytes()
                        .iter()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
                {
                    let mut bytes = Vec::with_capacity(32);
                    for pair in encoded.as_bytes().chunks_exact(2) {
                        let high = (pair[0] as char).to_digit(16).expect("validated hex digit");
                        let low = (pair[1] as char).to_digit(16).expect("validated hex digit");
                        bytes.push(((high << 4) | low) as u8);
                    }
                    Some(Box::new(Value::Blob(bytes)))
                } else {
                    None
                };
                Ok(Value::Option(decoded))
            }
            ("sha256" | "sha256_text" | "domain_sha256" | "to_hex" | "from_hex", _) => {
                Err(error("ORNA-EVAL-TYPE"))
            }
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    pub(crate) fn ovb_codec(
        &mut self,
        name: &str,
        values: Vec<Value>,
    ) -> Result<Value, EvaluationError> {
        match (name, values.as_slice()) {
            ("__encode", [value]) => {
                let bytes = value
                    .clone()
                    .canonical()?
                    .encode()
                    .map_err(|_| error("ORNA-EVAL-VALUE"))?;
                self.items(bytes.len())?;
                Ok(Value::Blob(bytes))
            }
            ("__decode", [Value::Blob(bytes)]) => {
                self.items(bytes.len())?;
                let value = CanonicalValue::decode(bytes).map_err(|_| error("ORNA-EVAL-VALUE"))?;
                Value::from_canonical(&value, self, 0)
            }
            ("__encode" | "__decode", _) => Err(error("ORNA-EVAL-TYPE")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    pub(crate) fn json_codec(
        &mut self,
        name: &str,
        values: Vec<Value>,
        nominals: &NominalDefinitions,
    ) -> Result<Value, EvaluationError> {
        match (name, values.as_slice()) {
            ("__encode", [value]) => {
                let mut output = String::new();
                write_json_value(value, &mut output, self, nominals, 0)?;
                self.string(output).map(Value::String)
            }
            ("__decode", [Value::String(input)])
            | ("__decode_with_options", [Value::String(input), Value::Bool(_)]) => {
                self.string(input.clone())?;
                let node = parse_json_node(input)?;
                json_node_to_value(node, self, 0)
            }
            ("__decode" | "__decode_with_options", _) => Err(error("ORNA-EVAL-TYPE")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    pub(crate) fn orna_codec(
        &mut self,
        name: &str,
        values: Vec<Value>,
    ) -> Result<Value, EvaluationError> {
        match (name, values.as_slice()) {
            ("__encode", [value]) => {
                let mut output = encode_orna_value(value, 0)?;
                output.push('\n');
                self.string(output).map(Value::String)
            }
            ("__decode", [Value::String(input)]) => {
                self.string(input.clone())?;
                let mut parsed = parse_expression(input);
                let mut bindings = BTreeMap::new();
                if !parsed.is_ok() || !admit_orna_data_expression(&mut parsed.value, &mut bindings)
                {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                let mut scope = Scope(
                    bindings,
                    BTreeSet::new(),
                    BTreeSet::new(),
                    NominalDefinitions::new(),
                    BTreeSet::new(),
                );
                self.evaluate(&parsed.value, &mut scope, 0)
            }
            ("__decode", _) => Err(error("ORNA-EVAL-TYPE")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
}

/// Strict RFC 4648 decoding is owned by `orna-value`; this wrapper only
/// maps its rejection onto the evaluator diagnostic.
pub(crate) fn decode_base64(input: &str) -> Result<Vec<u8>, EvaluationError> {
    orna_value::base64_decode(input).map_err(|_| error("ORNA-EVAL-VALUE"))
}
fn parse_json_raw(raw: &str) -> Result<JsonNode, EvaluationError> {
    let raw = raw.trim();
    match raw.as_bytes().first().copied() {
        Some(b'{') => {
            let RawJsonObject(fields) =
                serde_json::from_str(raw).map_err(|_| error("ORNA-EVAL-VALUE"))?;
            fields
                .into_iter()
                .map(|(key, value)| Ok((key, parse_json_raw(value.get())?)))
                .collect::<Result<Vec<_>, EvaluationError>>()
                .map(JsonNode::Object)
        }
        Some(b'[') => {
            let values: Vec<Box<RawValue>> =
                serde_json::from_str(raw).map_err(|_| error("ORNA-EVAL-VALUE"))?;
            values
                .into_iter()
                .map(|value| parse_json_raw(value.get()))
                .collect::<Result<Vec<_>, _>>()
                .map(JsonNode::Array)
        }
        Some(b'"') => serde_json::from_str(raw)
            .map(JsonNode::String)
            .map_err(|_| error("ORNA-EVAL-VALUE")),
        Some(b't' | b'f') => serde_json::from_str(raw)
            .map(JsonNode::Bool)
            .map_err(|_| error("ORNA-EVAL-VALUE")),
        Some(b'n') => serde_json::from_str::<()>(raw)
            .map(|()| JsonNode::Null)
            .map_err(|_| error("ORNA-EVAL-VALUE")),
        Some(_) => Ok(JsonNode::Number(raw.to_owned())),
        None => Err(error("ORNA-EVAL-VALUE")),
    }
}
pub(crate) fn parse_json_node(input: &str) -> Result<JsonNode, EvaluationError> {
    let raw: Box<RawValue> = serde_json::from_str(input).map_err(|_| error("ORNA-EVAL-VALUE"))?;
    parse_json_raw(raw.get())
}
pub(crate) fn json_node_to_value(
    node: JsonNode,
    context: &mut Context<'_, '_>,
    depth: usize,
) -> Result<Value, EvaluationError> {
    context.depth(depth)?;
    context.step()?;
    match node {
        JsonNode::Null => Ok(Value::Null),
        JsonNode::Bool(value) => Ok(Value::Bool(value)),
        JsonNode::String(value) => context.string(value).map(Value::String),
        JsonNode::Number(number) => {
            if number.contains(['.', 'e', 'E']) {
                let (mantissa, exponent) = match number.find(['e', 'E']) {
                    Some(index) => (&number[..index], &number[index..]),
                    None => (number.as_str(), ""),
                };
                let mantissa = if mantissa.contains('.') {
                    mantissa.to_owned()
                } else {
                    format!("{mantissa}.0")
                };
                let normalized = format!("{mantissa}{exponent}");
                let (coefficient, exponent) = parse_decimal(&normalized)?;
                DecimalValue::new(coefficient, exponent).map(Value::Decimal)
            } else {
                let integer = BigInt::parse_bytes(number.as_bytes(), 10)
                    .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                context.integer(integer).map(Value::Int)
            }
        }
        JsonNode::Array(values) => {
            context.items(values.len())?;
            values
                .into_iter()
                .map(|value| json_node_to_value(value, context, depth + 1))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::list)
        }
        JsonNode::Object(entries) => {
            context.items(entries.len())?;
            let mut fields = BTreeMap::new();
            for (key, value) in entries {
                context.string(key.clone())?;
                fields.insert(key, json_node_to_value(value, context, depth + 1)?);
            }
            Ok(Value::Record(fields))
        }
    }
}
fn write_json_value(
    value: &Value,
    output: &mut String,
    context: &mut Context<'_, '_>,
    nominals: &NominalDefinitions,
    depth: usize,
) -> Result<(), EvaluationError> {
    context.depth(depth)?;
    context.step()?;
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Int(value) => write!(output, "{value}").map_err(|_| error("ORNA-EVAL-LIMIT"))?,
        Value::Decimal(value) => {
            output.push_str(&decimal_json_token(value)?);
        }
        Value::Float(bits) => {
            let number = f64::from_bits(*bits);
            if !number.is_finite() {
                return Err(error("ORNA-EVAL-VALUE"));
            }
            output.push_str(&number.to_string());
        }
        Value::String(value) => {
            output.push_str(&serde_json::to_string(value).map_err(|_| error("ORNA-EVAL-VALUE"))?)
        }
        Value::List(_) | Value::Tuple(_) => {
            let values: &[Value] = match value {
                Value::List(values) => values,
                Value::Tuple(values) => values,
                _ => unreachable!("the arm matched a list or tuple"),
            };
            context.items(values.len())?;
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_json_value(value, output, context, nominals, depth + 1)?;
            }
            output.push(']');
        }
        Value::Record(fields) => {
            context.items(fields.len())?;
            output.push('{');
            for (index, (key, value)) in fields.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(&serde_json::to_string(key).map_err(|_| error("ORNA-EVAL-VALUE"))?);
                output.push(':');
                write_json_value(value, output, context, nominals, depth + 1)?;
            }
            output.push('}');
        }
        Value::Option(None) => output.push_str("null"),
        Value::Option(Some(value)) => {
            write_json_value(value, output, context, nominals, depth + 1)?;
        }
        Value::Money { amount, currency } => {
            let metadata = nominals
                .values()
                .find(|definition| object_id_key(&definition.type_id) == Some(*currency))
                .and_then(|definition| definition.currency.as_ref())
                .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
            output.push_str("{\"amount\":");
            let amount = decimal_json_token(amount)?;
            output.push_str(&serde_json::to_string(&amount).map_err(|_| error("ORNA-EVAL-VALUE"))?);
            output.push_str(",\"currency\":");
            output.push_str(
                &serde_json::to_string(&metadata.code).map_err(|_| error("ORNA-EVAL-VALUE"))?,
            );
            output.push('}');
        }
        Value::Unit
        | Value::Blob(_)
        | Value::AnnotatedBlob(_)
        | Value::ContextualBlob(_)
        | Value::Date(_)
        | Value::Uuid(_)
        | Value::Digest(_)
        | Value::Reference(_)
        | Value::TableReference(_)
        | Value::Instant { .. }
        | Value::Duration { .. }
        | Value::Period { .. }
        | Value::Error(_)
        | Value::Range { .. }
        | Value::Stream { .. }
        | Value::Relation(_)
        | Value::NominalRecord { .. }
        | Value::Enum { .. }
        | Value::Function { .. }
        | Value::Closure(_) => return Err(error("ORNA-EVAL-UNSUPPORTED")),
    }
    if output.len() > context.limits.max_string_bytes {
        return Err(error("ORNA-EVAL-LIMIT"));
    }
    Ok(())
}
fn decimal_json_token(value: &DecimalValue) -> Result<String, EvaluationError> {
    if value.coefficient.is_zero() {
        return Ok("0.0".to_owned());
    }
    let negative = value.coefficient.sign() == Sign::Minus;
    let digits = value.coefficient.abs().to_str_radix(10);
    let exponent = value
        .exponent10
        .to_i64()
        .filter(|exponent| exponent.unsigned_abs() <= DEFAULT_INTEGER_DIGITS as u64)
        .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
    let mut token = String::new();
    if negative {
        token.push('-');
    }
    if exponent >= 0 {
        token.push_str(&digits);
        token.push_str(&"0".repeat(exponent as usize));
        token.push_str(".0");
    } else {
        let point = i64::try_from(digits.len()).map_err(|_| error("ORNA-EVAL-LIMIT"))? + exponent;
        if point > 0 {
            let point = usize::try_from(point).map_err(|_| error("ORNA-EVAL-LIMIT"))?;
            token.push_str(&digits[..point]);
            token.push('.');
            token.push_str(&digits[point..]);
        } else {
            token.push_str("0.");
            token.push_str(&"0".repeat(point.unsigned_abs() as usize));
            token.push_str(&digits);
        }
    }
    Ok(token)
}
fn encode_orna_value(value: &Value, depth: usize) -> Result<String, EvaluationError> {
    if depth > DEFAULT_DEPTH {
        return Err(error("ORNA-EVAL-LIMIT"));
    }
    Ok(match value {
        Value::Null => "null".to_owned(),
        Value::Unit => "()".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Int(value) => value.to_string(),
        Value::Decimal(value) => format!("{}e{}.decimal", value.coefficient, value.exponent10),
        Value::Float(bits) => {
            let value = f64::from_bits(*bits);
            if value.is_nan() {
                "Float.nan".to_owned()
            } else if value == f64::INFINITY {
                "Float.infinity".to_owned()
            } else if value == f64::NEG_INFINITY {
                "-Float.infinity".to_owned()
            } else {
                let text = format!("{value:.16e}");
                let (mantissa, exponent) = text
                    .split_once('e')
                    .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
                let exponent = exponent
                    .parse::<i32>()
                    .map_err(|_| error("ORNA-EVAL-VALUE"))?;
                format!("{mantissa}e{exponent}f")
            }
        }
        Value::String(value) => encode_orna_string(value),
        Value::List(values) => format!(
            "[{}]",
            values
                .iter()
                .map(|value| encode_orna_value(value, depth + 1))
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        ),
        Value::Tuple(values) => {
            let mut parts = values
                .iter()
                .map(|value| encode_orna_value(value, depth + 1))
                .collect::<Result<Vec<_>, _>>()?;
            if parts.len() == 1 {
                parts[0].push(',');
            }
            format!("({})", parts.join(", "))
        }
        Value::Record(fields) => {
            if fields.is_empty() {
                "{}".to_owned()
            } else {
                let mut output = String::from("{\n");
                for (name, value) in fields {
                    if !is_identifier(name) {
                        return Err(error("ORNA-EVAL-UNSUPPORTED"));
                    }
                    output.push_str(&" ".repeat((depth + 1) * 2));
                    output.push_str(name);
                    output.push_str(": ");
                    output.push_str(&encode_orna_value(value, depth + 1)?);
                    output.push_str(",\n");
                }
                output.push_str(&" ".repeat(depth * 2));
                output.push('}');
                output
            }
        }
        Value::Option(None) => "null".to_owned(),
        Value::Option(Some(value)) => format!("Some({})", encode_orna_value(value, depth + 1)?),
        Value::Blob(value) => encode_orna_string(&orna_value::base64_encode(value)),
        Value::AnnotatedBlob(_) => return Err(error("ORNA-EVAL-UNSUPPORTED")),
        Value::ContextualBlob(_) => return Err(error("ORNA-EVAL-UNSUPPORTED")),
        Value::Date(value) => format!("date{}", encode_orna_string(value)),
        Value::Instant { .. } | Value::Duration { .. } | Value::Money { .. } => {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        Value::Uuid(value) => encode_orna_string(&format_uuid_bytes(value)),
        Value::Digest(value) => encode_orna_string(&format_digest_hex(value)),
        Value::TableReference(value) => encode_orna_string(&orna_value::base64_encode(
            &value.encode().map_err(|_| error("ORNA-EVAL-VALUE"))?,
        )),
        Value::Reference(value) => encode_orna_string(&orna_value::base64_encode(
            &value.encode().map_err(|_| error("ORNA-EVAL-VALUE"))?,
        )),
        Value::Period { .. }
        | Value::Error(_)
        | Value::Range { .. }
        | Value::Stream { .. }
        | Value::Relation(_)
        | Value::NominalRecord { .. }
        | Value::Enum { .. }
        | Value::Function { .. }
        | Value::Closure(_) => return Err(error("ORNA-EVAL-UNSUPPORTED")),
    })
}
fn encode_orna_string(value: &str) -> String {
    let mut output = String::from("\"");
    for scalar in value.chars() {
        match scalar {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '{' => output.push_str("\\u{7b}"),
            scalar if scalar.is_control() => {
                write!(output, "\\u{{{:x}}}", scalar as u32).expect("String writes succeed");
            }
            scalar => output.push(scalar),
        }
    }
    output.push('"');
    output
}
fn is_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_alphabetic())
        && chars.all(|scalar| scalar == '_' || scalar.is_alphanumeric())
}
/// The canonical nonfinite Float spellings (29-formats.md:125) and the scope binding carrying each.
/// The NUL prefix cannot occur in a source identifier, so the binding never collides with user names.
fn nonfinite_float_constant(expression: &Expr) -> Option<(&'static str, f64)> {
    let Expr::Field { base, name, .. } = expression else {
        return None;
    };
    if !matches!(base.as_ref(), Expr::Name { text, .. } if text == "Float") {
        return None;
    }
    match name.as_str() {
        "nan" => Some(("\0Float.nan", f64::NAN)),
        "infinity" => Some(("\0Float.infinity", f64::INFINITY)),
        _ => None,
    }
}
/// ORNA-FORMAT-004: admits only the data subset of canonical Orna text. A canonical nonfinite
/// Float spelling is rewritten to a binding holding its IEEE-754 value, so no `Float` path is
/// evaluated. Returns false for any construct that is not data.
fn admit_orna_data_expression(
    expression: &mut Expr,
    bindings: &mut BTreeMap<String, Value>,
) -> bool {
    if let Some((binding, value)) = nonfinite_float_constant(expression) {
        let span = expression.span();
        bindings.insert(binding.to_owned(), float_value(value));
        *expression = Expr::Name {
            text: binding.to_owned(),
            span,
        };
        return true;
    }
    match expression {
        Expr::Literal { .. } => true,
        Expr::Name { .. } => false,
        Expr::Group { inner, .. } => admit_orna_data_expression(inner, bindings),
        Expr::Unary { op, rhs, .. } => {
            matches!(op.as_str(), "+" | "-") && admit_orna_data_expression(rhs, bindings)
        }
        Expr::List { elements, .. } | Expr::Tuple { elements, .. } => elements
            .iter_mut()
            .all(|element| admit_orna_data_expression(element, bindings)),
        Expr::Record { fields, .. } => fields
            .iter_mut()
            .all(|field| admit_orna_data_expression(&mut field.value, bindings)),
        Expr::Field { base, .. } => matches!(
            &**base,
            Expr::Literal {
                kind: LiteralKind::Decimal | LiteralKind::Integer,
                ..
            }
        ),
        Expr::Call {
            callee, arguments, ..
        } if matches!(&**callee, Expr::Name { text, .. } if text == "Some")
            && arguments.len() == 1
            && arguments.iter().all(|argument| argument.name.is_none()) =>
        {
            arguments
                .iter_mut()
                .all(|argument| admit_orna_data_expression(&mut argument.value, bindings))
        }
        _ => false,
    }
}
pub(crate) fn format_uuid_bytes(bytes: &[u8; 16]) -> String {
    let mut output = String::with_capacity(36);
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            output.push('-');
        }
        write!(output, "{byte:02x}").expect("String writes succeed");
    }
    output
}
/// Renders a Digest as lowercase 64-hex; it is never an implicit string value.
pub(crate) fn format_digest_hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
