//! Closed CLI argument reflection and binding for `orna invoke` (ADR 0056).
//!
//! This module models the CLI's typed-argument binding step without touching
//! the sealed `sys.invoke` boundary. It reflects one resolved
//! [`FunctionDefinition`] signature, converts CLI strings to canonical typed
//! [`RuntimeValue`]s, and returns ordered [`InvocationArgument`]s in source
//! order. It never builds an `InvokeRequest` and never dispatches.
//!
//! The friendly `--<name>` form derives from each parameter's resolved source
//! name: a leading `p_` prefix is stripped, so `p_value` maps to `--value`.
//! The canonical `--arg <parameter>=<value>` form accepts either the opaque
//! canonical [`ParameterId`] text or the exact resolved source name.
//!
//! Supported string conversions (ADR 0056 "Reflection and binding"):
//!
//! | Parameter resolved type | CLI string form | Runtime value |
//! | --- | --- | --- |
//! | `INTEGER` | decimal integer | [`RuntimeValue::Integer`] |
//! | `BIGINT` | decimal integer | [`RuntimeValue::BigInt`] |
//! | `FLOAT` | finite decimal float | [`RuntimeValue::Float`] |
//! | `BOOLEAN` | `true` / `false` (case-sensitive) | [`RuntimeValue::Boolean`] |
//! | `TEXT` | literal text | [`RuntimeValue::Text`] |
//! | `BYTES` | base64 | [`RuntimeValue::Bytes`] |
//! | `UUID` | canonical UUID text | [`RuntimeValue::Bytes`] (16 raw bytes) |
//! | reference (`REF T`) | `@<type-name>/<object-id>` | [`RuntimeValue::Reference`] |
//!
//! A UUID value converts to its 16 raw bytes. orna-core cannot name the
//! standard `std.types.uuid` identity, so the caller resolves the UUID
//! value-type identity and representation when encoding through the sealed
//! route (ADR 0056 step 3). Named and opaque value types are likewise
//! caller-resolved: [`convert_cli_string`] reports
//! [`InvocationConversionError::UnsupportedType`] for `ResolvedType::Named`
//! and `ResolvedType::Value`, and the caller maps a resolved standard
//! value-type identity to its scalar form before converting.
//!
//! A reference type-name is validated structurally as a qualified name only;
//! orna-core cannot resolve names to type identities without a catalogue.
//! The caller may verify that the supplied type-name matches the resolved
//! reference target.

use std::{collections::HashSet, error::Error, fmt};

use base64::{Engine as _, engine::general_purpose::STANDARD};

use crate::{
    ObjectId, ParameterId, TypeId,
    catalogue::{FunctionDefinition, ParameterDefinition, QualifiedSemanticName},
    invocation::{InvocationArgument, InvocationParameterSelector, InvokeValue},
    types::{ResolvedType, StandardScalar},
    value::{RuntimeFloat, RuntimeValue},
};

/// One raw CLI argument submitted for binding.
///
/// The caller (the command parser) strips option prefixes and splits
/// `--arg <parameter>=<value>` pairs before constructing this model.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CliArgumentInput {
    /// The friendly `--<name> <value>` form; `name` is the bare flag name
    /// without the `--` prefix.
    Friendly {
        /// The bare friendly flag name.
        name: String,
        /// The CLI string to convert.
        value: String,
    },
    /// The canonical `--arg <parameter>=<value>` form; `parameter` is either
    /// the opaque canonical [`ParameterId`] text or the exact source name.
    Canonical {
        /// The canonical parameter selector.
        parameter: String,
        /// The CLI string to convert.
        value: String,
    },
    /// One non-flag CLI token; always rejected as a usage error.
    Positional(String),
}

/// One closed failure from binding CLI arguments against a function
/// signature. Each variant is the named usage-error class of ADR 0056; the
/// carried details are typed for diagnostics while the [`fmt::Display`] form stays
/// redacted for CLI usage.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvocationBindingError {
    /// The supplied friendly or canonical name does not uniquely identify a
    /// declared parameter. A signature whose derived friendly aliases collide
    /// (for example `p_x` and `x`) reports the ambiguous alias here.
    UnknownParameterName {
        /// The rejected friendly flag or canonical selector.
        name: String,
    },
    /// One parameter was supplied more than once.
    DuplicateParameter {
        /// The duplicated parameter identity.
        parameter: ParameterId,
        /// The resolved source name of the duplicated parameter.
        name: String,
    },
    /// A parameter without a default expression was not supplied.
    MissingRequiredParameter {
        /// The missing parameter identity.
        parameter: ParameterId,
        /// The resolved source name of the missing parameter.
        name: String,
    },
    /// A positional (non-flag) CLI token cannot bind to a parameter.
    UnexpectedArgument {
        /// The rejected positional token.
        argument: String,
    },
    /// A supplied string did not convert to the parameter's resolved type.
    ConversionFailed {
        /// The parameter whose value failed to convert.
        parameter: ParameterId,
        /// The resolved source name of the parameter.
        name: String,
        /// The typed conversion failure.
        detail: InvocationConversionError,
    },
}

impl fmt::Display for InvocationBindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownParameterName { name } => {
                write!(formatter, "unknown parameter `{name}`")
            }
            Self::DuplicateParameter { name, .. } => {
                write!(formatter, "parameter `{name}` supplied more than once")
            }
            Self::MissingRequiredParameter { name, .. } => {
                write!(formatter, "missing required parameter `{name}`")
            }
            Self::UnexpectedArgument { argument } => {
                write!(formatter, "unexpected positional argument `{argument}`")
            }
            Self::ConversionFailed { name, detail, .. } => {
                write!(
                    formatter,
                    "cannot convert value for parameter `{name}`: {detail}"
                )
            }
        }
    }
}

impl Error for InvocationBindingError {}

/// One typed failure from converting a CLI string to a [`RuntimeValue`].
#[non_exhaustive]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InvocationConversionError {
    /// The resolved type has no accepted CLI string form in this decision.
    UnsupportedType {
        /// The rejected resolved type descriptor.
        resolved_type: ResolvedType,
    },
    /// The value is empty for a type that requires at least one character.
    EmptyValue,
    /// An `INTEGER` or `BIGINT` string is not a decimal integer in range.
    InvalidInteger,
    /// A `FLOAT` string is not a finite decimal float.
    InvalidFloat,
    /// A `BOOLEAN` string is not exactly `true` or `false`.
    InvalidBoolean,
    /// A `BYTES` string is not valid base64.
    InvalidBase64,
    /// A `UUID` string is not canonical UUID text.
    InvalidUuid,
    /// A reference string is not the canonical `@<type-name>/<object-id>`
    /// form, or its object identity is not canonical opaque text.
    InvalidReference,
}

impl fmt::Display for InvocationConversionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedType { .. } => {
                formatter.write_str("the resolved type has no CLI string form in this decision")
            }
            Self::EmptyValue => formatter.write_str("the value is empty"),
            Self::InvalidInteger => formatter.write_str("expected a decimal integer"),
            Self::InvalidFloat => formatter.write_str("expected a finite decimal float"),
            Self::InvalidBoolean => formatter.write_str("expected `true` or `false`"),
            Self::InvalidBase64 => formatter.write_str("expected base64 text"),
            Self::InvalidUuid => formatter.write_str("expected canonical UUID text"),
            Self::InvalidReference => formatter
                .write_str("expected `@<type-name>/<object-id>` with a canonical object id"),
        }
    }
}

impl Error for InvocationConversionError {}

/// Reflects the function signature and binds CLI arguments into ordered typed
/// invocation arguments.
///
/// Each declared parameter may be supplied once, either by its friendly
/// `--<name>` form or by the canonical `--arg` form. Every parameter without
/// a default expression must be supplied. The returned arguments appear in
/// the order the inputs were supplied.
///
/// # Errors
///
/// Returns the first failure in input order: an unknown parameter name, a
/// duplicate parameter, a conversion failure, or an unexpected positional
/// argument. After all inputs are processed it reports the first missing
/// required parameter in declaration order.
pub fn bind_cli_arguments(
    definition: &FunctionDefinition,
    arguments: &[CliArgumentInput],
) -> Result<Vec<InvocationArgument>, InvocationBindingError> {
    let mut bound = HashSet::new();
    let mut ordered = Vec::with_capacity(arguments.len());

    for argument in arguments {
        let (parameter, value) = match argument {
            CliArgumentInput::Positional(token) => {
                return Err(InvocationBindingError::UnexpectedArgument {
                    argument: token.clone(),
                });
            }
            CliArgumentInput::Friendly { name, value } => {
                let parameter = resolve_friendly(definition, name).ok_or_else(|| {
                    InvocationBindingError::UnknownParameterName { name: name.clone() }
                })?;
                (parameter, value)
            }
            CliArgumentInput::Canonical { parameter, value } => {
                let parameter = resolve_canonical(definition, parameter).ok_or_else(|| {
                    InvocationBindingError::UnknownParameterName {
                        name: parameter.clone(),
                    }
                })?;
                (parameter, value)
            }
        };

        if !bound.insert(parameter.id()) {
            return Err(InvocationBindingError::DuplicateParameter {
                parameter: parameter.id(),
                name: parameter.name().to_owned(),
            });
        }

        let value = convert_cli_string(parameter.resolved_type(), value).map_err(|detail| {
            InvocationBindingError::ConversionFailed {
                parameter: parameter.id(),
                name: parameter.name().to_owned(),
                detail,
            }
        })?;

        ordered.push(InvocationArgument::new(
            InvocationParameterSelector::parameter_id(parameter.id()),
            InvokeValue::new(value)
                .expect("a flat CLI-converted value always fits the invocation carrier"),
        ));
    }

    for parameter in definition.parameters() {
        if parameter.default_expression().is_none() && !bound.contains(&parameter.id()) {
            return Err(InvocationBindingError::MissingRequiredParameter {
                parameter: parameter.id(),
                name: parameter.name().to_owned(),
            });
        }
    }

    Ok(ordered)
}

/// Converts one CLI string to a typed [`RuntimeValue`] for the ADR 0056
/// conversion table.
///
/// The caller selects the mapping by passing the resolved type. Standard
/// scalars convert directly; a typed reference uses the
/// `@<type-name>/<object-id>` canonical form. Named and value types have no
/// CLI string form in this decision and are caller-resolved.
///
/// # Errors
///
/// Returns [`InvocationConversionError::UnsupportedType`] for named, value,
/// and deferred scalar types; [`InvocationConversionError::EmptyValue`] for
/// an empty string on every non-`TEXT` form; and the matching typed failure
/// for each malformed scalar, base64, UUID, or reference string.
pub fn convert_cli_string(
    resolved_type: ResolvedType,
    input: &str,
) -> Result<RuntimeValue, InvocationConversionError> {
    match resolved_type {
        ResolvedType::Named(_) | ResolvedType::Value(_) => {
            Err(InvocationConversionError::UnsupportedType { resolved_type })
        }
        ResolvedType::Scalar(StandardScalar::CharacterLargeObject) => {
            Ok(RuntimeValue::Text(input.to_owned()))
        }
        ResolvedType::Scalar(_) if input.is_empty() => Err(InvocationConversionError::EmptyValue),
        ResolvedType::Reference { .. } if input.is_empty() => {
            Err(InvocationConversionError::EmptyValue)
        }
        ResolvedType::Reference { target } => convert_reference(target, input),
        ResolvedType::Scalar(StandardScalar::Integer) => input
            .parse::<i32>()
            .map(RuntimeValue::Integer)
            .map_err(|_| InvocationConversionError::InvalidInteger),
        ResolvedType::Scalar(StandardScalar::BigInt) => input
            .parse::<i64>()
            .map(RuntimeValue::BigInt)
            .map_err(|_| InvocationConversionError::InvalidInteger),
        ResolvedType::Scalar(StandardScalar::Float) => input
            .parse::<f64>()
            .map_err(|_| InvocationConversionError::InvalidFloat)
            .and_then(|value| {
                RuntimeFloat::new(value)
                    .map(RuntimeValue::Float)
                    .map_err(|_| InvocationConversionError::InvalidFloat)
            }),
        ResolvedType::Scalar(StandardScalar::Boolean) => match input {
            "true" => Ok(RuntimeValue::Boolean(true)),
            "false" => Ok(RuntimeValue::Boolean(false)),
            _ => Err(InvocationConversionError::InvalidBoolean),
        },
        ResolvedType::Scalar(StandardScalar::BinaryLargeObject) => STANDARD
            .decode(input)
            .map(RuntimeValue::Bytes)
            .map_err(|_| InvocationConversionError::InvalidBase64),
        ResolvedType::Scalar(StandardScalar::Uuid) => {
            let parsed =
                uuid::Uuid::parse_str(input).map_err(|_| InvocationConversionError::InvalidUuid)?;
            if parsed.to_string() != input {
                return Err(InvocationConversionError::InvalidUuid);
            }
            Ok(RuntimeValue::Bytes(parsed.as_bytes().to_vec()))
        }
        ResolvedType::Scalar(
            StandardScalar::Decimal
            | StandardScalar::Date
            | StandardScalar::Time
            | StandardScalar::Timestamp
            | StandardScalar::Duration
            | StandardScalar::Void,
        ) => Err(InvocationConversionError::UnsupportedType { resolved_type }),
    }
}

/// Converts one canonical `@<type-name>/<object-id>` reference string.
///
/// The type-name is validated structurally as a qualified name; the object
/// identity must be canonical opaque [`ObjectId`] text. orna-core cannot
/// resolve the type-name to the resolved target identity, so the caller may
/// verify that match after binding.
fn convert_reference(
    target: TypeId,
    input: &str,
) -> Result<RuntimeValue, InvocationConversionError> {
    let Some(rest) = input.strip_prefix('@') else {
        return Err(InvocationConversionError::InvalidReference);
    };
    let Some((type_name, object_id)) = rest.split_once('/') else {
        return Err(InvocationConversionError::InvalidReference);
    };
    if QualifiedSemanticName::new(type_name.split('.')).is_err() {
        return Err(InvocationConversionError::InvalidReference);
    }
    let object = ObjectId::from_canonical(object_id)
        .map_err(|_| InvocationConversionError::InvalidReference)?;
    Ok(RuntimeValue::Reference { target, object })
}

/// The friendly flag for one parameter: the resolved source name with a
/// leading `p_` prefix stripped.
fn friendly_name(name: &str) -> &str {
    name.strip_prefix("p_").unwrap_or(name)
}

/// Resolves a friendly flag to the unique parameter it names.
///
/// A derived friendly alias that matches more than one parameter (for example
/// `p_x` and `x` in one signature) cannot name a parameter and resolves to
/// `None`.
fn resolve_friendly<'a>(
    definition: &'a FunctionDefinition,
    name: &str,
) -> Option<&'a ParameterDefinition> {
    let mut matches = definition
        .parameters()
        .iter()
        .filter(|parameter| friendly_name(parameter.name()) == name);
    let matched = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(matched)
}

/// Resolves a canonical `--arg` selector: the opaque canonical [`ParameterId`]
/// text when it parses, otherwise the exact resolved source name.
fn resolve_canonical<'a>(
    definition: &'a FunctionDefinition,
    selector: &str,
) -> Option<&'a ParameterDefinition> {
    if let Ok(id) = ParameterId::from_canonical(selector) {
        return definition.parameter_by_id(id);
    }
    definition.parameter_by_name(selector)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ExpressionId, FunctionId, FunctionRevisionId, TypeId,
        catalogue::{
            FunctionDomain, FunctionReturn, FunctionSecurity, FunctionTransaction,
            FunctionVolatility,
        },
    };

    fn name(parts: &[&str]) -> QualifiedSemanticName {
        QualifiedSemanticName::new(parts.iter().copied()).unwrap()
    }

    fn parameter(
        id: u8,
        name: &str,
        ordinal: u32,
        resolved_type: ResolvedType,
        default: Option<ExpressionId>,
    ) -> ParameterDefinition {
        ParameterDefinition::new(
            ParameterId::from_bytes([id; 16]),
            name,
            ordinal,
            resolved_type,
            default,
        )
    }

    fn function(parameters: Vec<ParameterDefinition>) -> FunctionDefinition {
        FunctionDefinition::new(
            FunctionId::from_bytes([7; 16]),
            name(&["cli", "bind"]),
            FunctionDomain::Server,
            parameters,
            FunctionReturn::Single(ResolvedType::scalar(StandardScalar::Void)),
            FunctionRevisionId::from_bytes([9; 16]),
            FunctionSecurity::Invoker,
            Some(FunctionTransaction::ReadOnly),
            FunctionVolatility::Volatile,
        )
    }

    fn friendly(name: &str, value: &str) -> CliArgumentInput {
        CliArgumentInput::Friendly {
            name: name.to_owned(),
            value: value.to_owned(),
        }
    }

    fn canonical(parameter: &str, value: &str) -> CliArgumentInput {
        CliArgumentInput::Canonical {
            parameter: parameter.to_owned(),
            value: value.to_owned(),
        }
    }

    fn positional(token: &str) -> CliArgumentInput {
        CliArgumentInput::Positional(token.to_owned())
    }

    fn value(argument: &InvocationArgument) -> &RuntimeValue {
        argument.value().value()
    }

    fn float(value: f64) -> RuntimeValue {
        RuntimeValue::Float(RuntimeFloat::new(value).unwrap())
    }

    #[test]
    fn converts_integer_and_bigint_decimal_strings() {
        assert_eq!(
            convert_cli_string(ResolvedType::scalar(StandardScalar::Integer), "0").unwrap(),
            RuntimeValue::Integer(0)
        );
        assert_eq!(
            convert_cli_string(ResolvedType::scalar(StandardScalar::Integer), "-42").unwrap(),
            RuntimeValue::Integer(-42)
        );
        assert_eq!(
            convert_cli_string(ResolvedType::scalar(StandardScalar::Integer), "2147483647")
                .unwrap(),
            RuntimeValue::Integer(i32::MAX)
        );
        assert_eq!(
            convert_cli_string(ResolvedType::scalar(StandardScalar::BigInt), "-7").unwrap(),
            RuntimeValue::BigInt(-7)
        );
        assert_eq!(
            convert_cli_string(
                ResolvedType::scalar(StandardScalar::BigInt),
                "9223372036854775807"
            )
            .unwrap(),
            RuntimeValue::BigInt(i64::MAX)
        );
    }

    #[test]
    fn converts_float_decimal_strings() {
        for (input, expected) in [
            ("3.5", 3.5),
            ("-0.25", -0.25),
            ("1e3", 1000.0),
            ("2.5e-2", 0.025),
        ] {
            assert_eq!(
                convert_cli_string(ResolvedType::scalar(StandardScalar::Float), input).unwrap(),
                float(expected),
                "converting {input}"
            );
        }
    }

    #[test]
    fn converts_boolean_true_and_false_only() {
        assert_eq!(
            convert_cli_string(ResolvedType::scalar(StandardScalar::Boolean), "true").unwrap(),
            RuntimeValue::Boolean(true)
        );
        assert_eq!(
            convert_cli_string(ResolvedType::scalar(StandardScalar::Boolean), "false").unwrap(),
            RuntimeValue::Boolean(false)
        );
    }

    #[test]
    fn rejects_wrong_case_boolean() {
        for input in ["TRUE", "True", "FALSE", "1", "yes", "t"] {
            assert_eq!(
                convert_cli_string(ResolvedType::scalar(StandardScalar::Boolean), input),
                Err(InvocationConversionError::InvalidBoolean),
                "rejecting {input}"
            );
        }
    }

    #[test]
    fn converts_text_literal_including_empty() {
        assert_eq!(
            convert_cli_string(
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                "hello world"
            )
            .unwrap(),
            RuntimeValue::Text("hello world".to_owned())
        );
        assert_eq!(
            convert_cli_string(
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                ""
            )
            .unwrap(),
            RuntimeValue::Text(String::new())
        );
    }

    #[test]
    fn converts_bytes_base64() {
        assert_eq!(
            convert_cli_string(
                ResolvedType::scalar(StandardScalar::BinaryLargeObject),
                "SGVsbG8="
            )
            .unwrap(),
            RuntimeValue::Bytes(b"Hello".to_vec())
        );
        assert_eq!(
            convert_cli_string(
                ResolvedType::scalar(StandardScalar::BinaryLargeObject),
                "AAECAw=="
            )
            .unwrap(),
            RuntimeValue::Bytes(vec![0, 1, 2, 3])
        );
    }

    #[test]
    fn rejects_malformed_base64() {
        assert_eq!(
            convert_cli_string(
                ResolvedType::scalar(StandardScalar::BinaryLargeObject),
                "not base64!!!"
            ),
            Err(InvocationConversionError::InvalidBase64)
        );
        assert_eq!(
            convert_cli_string(
                ResolvedType::scalar(StandardScalar::BinaryLargeObject),
                "SGVsbG8"
            ),
            Err(InvocationConversionError::InvalidBase64)
        );
    }

    #[test]
    fn converts_uuid_canonical_text_to_sixteen_raw_bytes() {
        assert_eq!(
            convert_cli_string(
                ResolvedType::scalar(StandardScalar::Uuid),
                "123e4567-e89b-12d3-a456-426614174000"
            )
            .unwrap(),
            RuntimeValue::Bytes(vec![
                0x12, 0x3e, 0x45, 0x67, 0xe8, 0x9b, 0x12, 0xd3, 0xa4, 0x56, 0x42, 0x66, 0x14, 0x17,
                0x40, 0x00,
            ])
        );
    }

    #[test]
    fn rejects_non_canonical_uuid_text() {
        for input in [
            "not-a-uuid",
            "123e4567e89b12d3a456426614174000",
            "123E4567-E89B-12D3-A456-426614174000",
            "{123e4567-e89b-12d3-a456-426614174000}",
        ] {
            assert_eq!(
                convert_cli_string(ResolvedType::scalar(StandardScalar::Uuid), input),
                Err(InvocationConversionError::InvalidUuid),
                "rejecting {input}"
            );
        }
    }

    #[test]
    fn converts_reference_canonical_form() {
        let target = TypeId::from_bytes([4; 16]);
        let object = ObjectId::from_bytes([5; 16]);
        let input = format!("@std.types.task/{}", object.canonical());
        assert_eq!(
            convert_cli_string(ResolvedType::reference(target), &input).unwrap(),
            RuntimeValue::Reference { target, object }
        );
    }

    #[test]
    fn rejects_malformed_reference_forms() {
        let target = TypeId::from_bytes([4; 16]);
        let object = ObjectId::from_bytes([5; 16]);
        let object_text = object.canonical();
        for input in [
            "no-at-sign".to_owned(),
            "@nope".to_owned(),
            format!("@/{}", object_text),
            format!("@std.types.task/x/{}", object_text),
            format!("@std..task/{}", object_text),
            "@std.types.task/not-an-object".to_owned(),
            format!("@std.types.task/{}x", object_text),
        ] {
            assert_eq!(
                convert_cli_string(ResolvedType::reference(target), &input),
                Err(InvocationConversionError::InvalidReference),
                "rejecting {input}"
            );
        }
    }

    #[test]
    fn rejects_named_and_value_types_without_a_cli_form() {
        let named = TypeId::from_bytes([1; 16]);
        let value = TypeId::from_bytes([2; 16]);
        assert_eq!(
            convert_cli_string(ResolvedType::named(named), "anything"),
            Err(InvocationConversionError::UnsupportedType {
                resolved_type: ResolvedType::named(named)
            })
        );
        assert_eq!(
            convert_cli_string(ResolvedType::value(value), "anything"),
            Err(InvocationConversionError::UnsupportedType {
                resolved_type: ResolvedType::value(value)
            })
        );
    }

    #[test]
    fn rejects_deferred_scalar_forms() {
        for scalar in [
            StandardScalar::Decimal,
            StandardScalar::Date,
            StandardScalar::Time,
            StandardScalar::Timestamp,
            StandardScalar::Duration,
            StandardScalar::Void,
        ] {
            let resolved = ResolvedType::scalar(scalar);
            assert_eq!(
                convert_cli_string(resolved, "anything"),
                Err(InvocationConversionError::UnsupportedType {
                    resolved_type: resolved
                }),
                "rejecting {scalar:?}"
            );
        }
    }

    #[test]
    fn rejects_empty_value_for_every_non_text_form() {
        for scalar in [
            StandardScalar::Integer,
            StandardScalar::BigInt,
            StandardScalar::Float,
            StandardScalar::Boolean,
            StandardScalar::BinaryLargeObject,
            StandardScalar::Uuid,
        ] {
            assert_eq!(
                convert_cli_string(ResolvedType::scalar(scalar), ""),
                Err(InvocationConversionError::EmptyValue),
                "rejecting empty {scalar:?}"
            );
        }
        assert_eq!(
            convert_cli_string(ResolvedType::reference(TypeId::from_bytes([4; 16])), ""),
            Err(InvocationConversionError::EmptyValue)
        );
    }

    #[test]
    fn binds_friendly_arguments_in_source_order() {
        let count_id = ParameterId::from_bytes([1; 16]);
        let label_id = ParameterId::from_bytes([2; 16]);
        let definition = function(vec![
            parameter(
                1,
                "p_count",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                None,
            ),
            parameter(
                2,
                "p_label",
                1,
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                None,
            ),
        ]);

        let bound = bind_cli_arguments(
            &definition,
            &[friendly("label", "hi"), friendly("count", "42")],
        )
        .unwrap();

        assert_eq!(bound.len(), 2);
        assert_eq!(
            bound[0].selector(),
            &InvocationParameterSelector::parameter_id(label_id)
        );
        assert_eq!(value(&bound[0]), &RuntimeValue::Text("hi".to_owned()));
        assert_eq!(
            bound[1].selector(),
            &InvocationParameterSelector::parameter_id(count_id)
        );
        assert_eq!(value(&bound[1]), &RuntimeValue::Integer(42));
    }

    #[test]
    fn binds_canonical_selector_by_opaque_id_and_by_source_name() {
        let count_id = ParameterId::from_bytes([1; 16]);
        let definition = function(vec![parameter(
            1,
            "p_count",
            0,
            ResolvedType::scalar(StandardScalar::Integer),
            None,
        )]);

        let by_id =
            bind_cli_arguments(&definition, &[canonical(&count_id.canonical(), "1")]).unwrap();
        assert_eq!(
            by_id[0].selector(),
            &InvocationParameterSelector::parameter_id(count_id)
        );

        let by_name = bind_cli_arguments(&definition, &[canonical("p_count", "2")]).unwrap();
        assert_eq!(
            by_name[0].selector(),
            &InvocationParameterSelector::parameter_id(count_id)
        );
        assert_eq!(value(&by_name[0]), &RuntimeValue::Integer(2));
    }

    #[test]
    fn accepts_omitted_parameter_with_a_default() {
        let definition = function(vec![
            parameter(
                1,
                "p_count",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                None,
            ),
            parameter(
                2,
                "p_note",
                1,
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                Some(ExpressionId::from_bytes([0x82; 16])),
            ),
        ]);

        let bound = bind_cli_arguments(&definition, &[friendly("count", "3")]).unwrap();
        assert_eq!(bound.len(), 1);
        assert_eq!(
            bound[0].selector(),
            &InvocationParameterSelector::parameter_id(ParameterId::from_bytes([1; 16]))
        );
        assert_eq!(value(&bound[0]), &RuntimeValue::Integer(3));
    }

    #[test]
    fn rejects_unknown_friendly_parameter_name() {
        let definition = function(vec![parameter(
            1,
            "p_count",
            0,
            ResolvedType::scalar(StandardScalar::Integer),
            None,
        )]);

        assert_eq!(
            bind_cli_arguments(&definition, &[friendly("nope", "1")]),
            Err(InvocationBindingError::UnknownParameterName {
                name: "nope".to_owned()
            })
        );
    }

    #[test]
    fn rejects_unknown_canonical_parameter_name() {
        let definition = function(vec![parameter(
            1,
            "p_count",
            0,
            ResolvedType::scalar(StandardScalar::Integer),
            None,
        )]);

        assert_eq!(
            bind_cli_arguments(&definition, &[canonical("p_nope", "1")]),
            Err(InvocationBindingError::UnknownParameterName {
                name: "p_nope".to_owned()
            })
        );

        let foreign = ParameterId::from_bytes([0x99; 16]).canonical();
        assert_eq!(
            bind_cli_arguments(&definition, &[canonical(&foreign, "1")]),
            Err(InvocationBindingError::UnknownParameterName { name: foreign })
        );
    }

    #[test]
    fn rejects_duplicate_parameter_across_forms() {
        let definition = function(vec![parameter(
            1,
            "p_count",
            0,
            ResolvedType::scalar(StandardScalar::Integer),
            None,
        )]);

        assert_eq!(
            bind_cli_arguments(
                &definition,
                &[friendly("count", "1"), friendly("count", "2")]
            ),
            Err(InvocationBindingError::DuplicateParameter {
                parameter: ParameterId::from_bytes([1; 16]),
                name: "p_count".to_owned()
            })
        );
        assert_eq!(
            bind_cli_arguments(
                &definition,
                &[friendly("count", "1"), canonical("p_count", "2")]
            ),
            Err(InvocationBindingError::DuplicateParameter {
                parameter: ParameterId::from_bytes([1; 16]),
                name: "p_count".to_owned()
            })
        );
    }

    #[test]
    fn rejects_missing_required_parameter_in_declaration_order() {
        let definition = function(vec![
            parameter(
                1,
                "p_first",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                None,
            ),
            parameter(
                2,
                "p_second",
                1,
                ResolvedType::scalar(StandardScalar::Integer),
                None,
            ),
        ]);

        assert_eq!(
            bind_cli_arguments(&definition, &[friendly("second", "2")]),
            Err(InvocationBindingError::MissingRequiredParameter {
                parameter: ParameterId::from_bytes([1; 16]),
                name: "p_first".to_owned()
            })
        );
    }

    #[test]
    fn rejects_extra_positional_argument() {
        let definition = function(vec![parameter(
            1,
            "p_count",
            0,
            ResolvedType::scalar(StandardScalar::Integer),
            None,
        )]);

        assert_eq!(
            bind_cli_arguments(&definition, &[friendly("count", "1"), positional("stray")]),
            Err(InvocationBindingError::UnexpectedArgument {
                argument: "stray".to_owned()
            })
        );
        assert_eq!(
            bind_cli_arguments(&definition, &[positional("stray")]),
            Err(InvocationBindingError::UnexpectedArgument {
                argument: "stray".to_owned()
            })
        );
    }

    #[test]
    fn rejects_conversion_failure_per_type() {
        let definition = |scalar| {
            function(vec![parameter(
                1,
                "p_value",
                0,
                ResolvedType::scalar(scalar),
                None,
            )])
        };

        let cases = [
            (
                StandardScalar::Integer,
                "12x",
                InvocationConversionError::InvalidInteger,
            ),
            (
                StandardScalar::Integer,
                "2147483648",
                InvocationConversionError::InvalidInteger,
            ),
            (
                StandardScalar::BigInt,
                "12x",
                InvocationConversionError::InvalidInteger,
            ),
            (
                StandardScalar::BigInt,
                "9223372036854775808",
                InvocationConversionError::InvalidInteger,
            ),
            (
                StandardScalar::Float,
                "12.5.5",
                InvocationConversionError::InvalidFloat,
            ),
            (
                StandardScalar::Float,
                "inf",
                InvocationConversionError::InvalidFloat,
            ),
            (
                StandardScalar::Float,
                "NaN",
                InvocationConversionError::InvalidFloat,
            ),
            (
                StandardScalar::Boolean,
                "TRUE",
                InvocationConversionError::InvalidBoolean,
            ),
            (
                StandardScalar::BinaryLargeObject,
                "not base64!!!",
                InvocationConversionError::InvalidBase64,
            ),
            (
                StandardScalar::Uuid,
                "not-a-uuid",
                InvocationConversionError::InvalidUuid,
            ),
        ];

        for (scalar, input, detail) in cases {
            assert_eq!(
                bind_cli_arguments(&definition(scalar), &[friendly("value", input)]),
                Err(InvocationBindingError::ConversionFailed {
                    parameter: ParameterId::from_bytes([1; 16]),
                    name: "p_value".to_owned(),
                    detail,
                }),
                "rejecting {input} for {scalar:?}"
            );
        }
    }

    #[test]
    fn rejects_conversion_failure_for_reference_parameter() {
        let definition = function(vec![parameter(
            1,
            "p_owner",
            0,
            ResolvedType::reference(TypeId::from_bytes([4; 16])),
            None,
        )]);

        assert_eq!(
            bind_cli_arguments(&definition, &[friendly("owner", "@nope")]),
            Err(InvocationBindingError::ConversionFailed {
                parameter: ParameterId::from_bytes([1; 16]),
                name: "p_owner".to_owned(),
                detail: InvocationConversionError::InvalidReference,
            })
        );
    }

    #[test]
    fn rejects_empty_value_through_binding() {
        let definition = function(vec![parameter(
            1,
            "p_count",
            0,
            ResolvedType::scalar(StandardScalar::Integer),
            None,
        )]);

        assert_eq!(
            bind_cli_arguments(&definition, &[friendly("count", "")]),
            Err(InvocationBindingError::ConversionFailed {
                parameter: ParameterId::from_bytes([1; 16]),
                name: "p_count".to_owned(),
                detail: InvocationConversionError::EmptyValue,
            })
        );
    }

    #[test]
    fn reports_first_failure_in_source_order() {
        let definition = function(vec![parameter(
            1,
            "p_count",
            0,
            ResolvedType::scalar(StandardScalar::Integer),
            None,
        )]);

        assert_eq!(
            bind_cli_arguments(
                &definition,
                &[friendly("nope", "1"), friendly("count", "x")]
            ),
            Err(InvocationBindingError::UnknownParameterName {
                name: "nope".to_owned()
            })
        );
    }

    #[test]
    fn colliding_friendly_alias_is_an_unknown_name() {
        let definition = function(vec![
            parameter(
                1,
                "p_value",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                None,
            ),
            parameter(
                2,
                "value",
                1,
                ResolvedType::scalar(StandardScalar::Integer),
                None,
            ),
        ]);

        assert_eq!(
            bind_cli_arguments(&definition, &[friendly("value", "1")]),
            Err(InvocationBindingError::UnknownParameterName {
                name: "value".to_owned()
            })
        );
    }

    #[test]
    fn binds_a_mixed_signature_with_all_adr_forms() {
        let definition = function(vec![
            parameter(
                1,
                "p_count",
                0,
                ResolvedType::scalar(StandardScalar::Integer),
                None,
            ),
            parameter(
                2,
                "p_big",
                1,
                ResolvedType::scalar(StandardScalar::BigInt),
                None,
            ),
            parameter(
                3,
                "p_ratio",
                2,
                ResolvedType::scalar(StandardScalar::Float),
                None,
            ),
            parameter(
                4,
                "p_active",
                3,
                ResolvedType::scalar(StandardScalar::Boolean),
                None,
            ),
            parameter(
                5,
                "p_label",
                4,
                ResolvedType::scalar(StandardScalar::CharacterLargeObject),
                None,
            ),
            parameter(
                6,
                "p_blob",
                5,
                ResolvedType::scalar(StandardScalar::BinaryLargeObject),
                None,
            ),
            parameter(
                7,
                "p_uuid",
                6,
                ResolvedType::scalar(StandardScalar::Uuid),
                None,
            ),
            parameter(
                8,
                "p_owner",
                7,
                ResolvedType::reference(TypeId::from_bytes([4; 16])),
                None,
            ),
        ]);

        let owner = ObjectId::from_bytes([5; 16]);
        let bound = bind_cli_arguments(
            &definition,
            &[
                friendly("uuid", "123e4567-e89b-12d3-a456-426614174000"),
                friendly("owner", &format!("@std.types.task/{}", owner.canonical())),
                friendly("active", "false"),
                canonical("p_blob", "SGVsbG8="),
                friendly("label", "hello"),
                friendly("ratio", "2.5"),
                friendly("big", "9007199254740993"),
                friendly("count", "7"),
            ],
        )
        .unwrap();

        let expected = [
            RuntimeValue::Bytes(vec![
                0x12, 0x3e, 0x45, 0x67, 0xe8, 0x9b, 0x12, 0xd3, 0xa4, 0x56, 0x42, 0x66, 0x14, 0x17,
                0x40, 0x00,
            ]),
            RuntimeValue::Reference {
                target: TypeId::from_bytes([4; 16]),
                object: owner,
            },
            RuntimeValue::Boolean(false),
            RuntimeValue::Bytes(b"Hello".to_vec()),
            RuntimeValue::Text("hello".to_owned()),
            float(2.5),
            RuntimeValue::BigInt(9_007_199_254_740_993),
            RuntimeValue::Integer(7),
        ];
        let ids = [
            [7; 16], [8; 16], [4; 16], [6; 16], [5; 16], [3; 16], [2; 16], [1; 16],
        ];

        assert_eq!(bound.len(), expected.len());
        for (argument, (expected, id)) in bound.iter().zip(expected.iter().zip(ids)) {
            assert_eq!(
                argument.selector(),
                &InvocationParameterSelector::parameter_id(ParameterId::from_bytes(id))
            );
            assert_eq!(value(argument), expected);
        }
    }
}
