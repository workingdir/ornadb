//! A deliberately small, deterministic Orna 1.0 expression evaluator.
//!
//! The public boundary admits and returns only OVB-1 canonical values. It has
//! no I/O, external mutation, clock, random, module-loading, or host-call
//! capability.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    rc::Rc,
};

use num_bigint::{BigInt, Sign};
use num_integer::Integer;
use num_traits::{Signed, ToPrimitive, Zero};
use orna_foundation_v1::{CanonicalValue, Diagnostic, DiagnosticSeverity, SafeText};
use orna_semantic_v1::StandardDependencyProfile;
use orna_syntax_v1::{
    AssignmentOperator, AssignmentTarget, ControlKind, Expr, LiteralKind, Parameter, Pattern,
    PatternField, ReplInput, Statement, StringSegment, parse_expression, parse_repl,
};
use orna_value_v1::{CANONICAL_NAN_BITS, Raw, float_max, float_min, float_ordinary_eq};
use unicode_normalization::UnicodeNormalization;

mod admitted_repl;
mod repl;

pub use admitted_repl::{AdmittedReplSession, ReplError};
pub use repl::{ReplSession, parse_admitted_repl};

/// The verified standard-source bundle used by the bounded local and remote
/// REPL boundaries. The source is included from the same canonical module
/// file as the executable local REPL, while this crate owns verification of
/// its profile before either boundary admits an import.
const REFERENCE_STD_MATH_LOGICAL_PATH: &str = "std/math.orna";
const REFERENCE_STD_MATH_SOURCE: &str = include_str!("../../orna-cli-v1/src/stdlib/std/math.orna");

/// Returns the reference standard sources supplied to the bounded REPL.
#[must_use]
pub fn reference_standard_sources() -> [(String, String); 1] {
    [(
        REFERENCE_STD_MATH_LOGICAL_PATH.into(),
        REFERENCE_STD_MATH_SOURCE.into(),
    )]
}

/// Returns the immutable profile that verifies [`reference_standard_sources`].
#[must_use]
pub fn reference_standard_profile() -> StandardDependencyProfile {
    StandardDependencyProfile::from_sources("orna.std/v1-pure-math", reference_standard_sources())
        .expect("the bundled reference standard sources are valid")
}

const DEFAULT_SOURCE_BYTES: usize = 65_536;
const DEFAULT_STEPS: u64 = 10_000;
const DEFAULT_DEPTH: usize = 64;
const DEFAULT_ITEMS: usize = 1_024;
const DEFAULT_STRING_BYTES: usize = 16_384;
const DEFAULT_INTEGER_DIGITS: usize = 1_024;

/// Explicit resource bounds. All zero values reject evaluation immediately.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub max_source_bytes: usize,
    pub max_steps: u64,
    pub max_depth: usize,
    pub max_collection_items: usize,
    pub max_string_bytes: usize,
    pub max_integer_digits: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_source_bytes: DEFAULT_SOURCE_BYTES,
            max_steps: DEFAULT_STEPS,
            max_depth: DEFAULT_DEPTH,
            max_collection_items: DEFAULT_ITEMS,
            max_string_bytes: DEFAULT_STRING_BYTES,
            max_integer_digits: DEFAULT_INTEGER_DIGITS,
        }
    }
}

impl Limits {
    /// Validate configuration and source bytes before a caller parses source.
    pub fn check_source(self, source: &str) -> Result<(), EvaluationError> {
        check_limits(source, self)
    }

    /// Validate configuration and a retained collection's total item count.
    pub fn check_items(self, count: usize) -> Result<(), EvaluationError> {
        validate_limits(self)?;
        if count > self.max_collection_items {
            Err(error("ORNA-EVAL-LIMIT"))
        } else {
            Ok(())
        }
    }

    /// Validate an integer produced at an evaluation boundary.
    pub fn check_integer(self, value: &BigInt) -> Result<(), EvaluationError> {
        validate_limits(self)?;
        if value.to_str_radix(10).len() > self.max_integer_digits {
            Err(error("ORNA-EVAL-LIMIT"))
        } else {
            Ok(())
        }
    }
}

/// A deterministic name environment. Values must be canonical OVB-1 values.
/// Qualified enum-label patterns resolve an exact `Type.variant` binding here;
/// its enum type and variant identities are matched before payload fields bind.
pub type Environment = BTreeMap<String, CanonicalValue>;

/// An admitted pure function and its lexical immutable value environment.
#[derive(Clone, Debug)]
pub struct PureFunction {
    pub parameters: Vec<Parameter>,
    pub body: Expr,
    pub environment: Environment,
}

/// Explicitly admitted named functions; no host or module lookup is performed.
pub type Functions = BTreeMap<String, PureFunction>;

/// A payload-free, stable failure suitable for conformance adapters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvaluationError {
    diagnostic: Box<Diagnostic>,
}

impl EvaluationError {
    /// Constructs a payload-free error from an already-admitted safe code.
    /// Effect handlers cannot attach source, argument, or host payloads.
    pub fn redacted(code: SafeText) -> Self {
        Self {
            diagnostic: Box::new(
                Diagnostic::new(code, DiagnosticSeverity::Error, SafeText::redacted())
                    .expect("safe diagnostic code")
                    .redacted(),
            ),
        }
    }
    pub fn diagnostic(&self) -> &Diagnostic {
        &self.diagnostic
    }
    pub fn code(&self) -> &str {
        self.diagnostic.code()
    }
}
impl fmt::Display for EvaluationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.diagnostic.code())
    }
}
impl std::error::Error for EvaluationError {}

/// Executes an admitted effect after its arguments have been evaluated exactly
/// once into canonical values. Returning `Ok(None)` leaves the call to the
/// ordinary pure evaluator.
pub trait EffectHandler {
    fn handle(
        &mut self,
        callee: &Expr,
        arguments: &[CanonicalValue],
    ) -> Result<Option<CanonicalValue>, EvaluationError>;
}

/// Evaluate one expression using [`parse_expression`].
pub fn evaluate_expression(
    source: &str,
    environment: &Environment,
    limits: Limits,
) -> Result<CanonicalValue, EvaluationError> {
    evaluate_expression_with_functions(source, environment, &Functions::new(), limits)
}

/// Evaluate source against explicit values and pure functions, checking source
/// limits before parsing. No module or host lookup is performed.
pub fn evaluate_expression_with_functions(
    source: &str,
    environment: &Environment,
    functions: &Functions,
    limits: Limits,
) -> Result<CanonicalValue, EvaluationError> {
    check_limits(source, limits)?;
    let parsed = parse_expression(source);
    if !parsed.is_ok() {
        return Err(error("ORNA-EVAL-PARSE"));
    }
    evaluate_with_functions(&parsed.value, environment, functions, limits)
}

/// Evaluate a REPL expression using [`parse_repl`]. REPL declarations are
/// intentionally outside this evaluator's side-effect-free subset.
pub fn evaluate_repl(
    source: &str,
    environment: &Environment,
    limits: Limits,
) -> Result<CanonicalValue, EvaluationError> {
    check_limits(source, limits)?;
    let parsed = parse_repl(source);
    if !parsed.is_ok() {
        return Err(error("ORNA-EVAL-PARSE"));
    }
    match parsed.value {
        ReplInput::Expression(expression) => evaluate_parsed(&expression, environment, limits),
        ReplInput::Item(_) => Err(error("ORNA-EVAL-UNSUPPORTED")),
    }
}

/// Evaluate an already parsed expression. This is the conformance integration
/// seam; the source API above is merely a parser adapter.
pub fn evaluate_parsed(
    expression: &Expr,
    environment: &Environment,
    limits: Limits,
) -> Result<CanonicalValue, EvaluationError> {
    evaluate_with_functions(expression, environment, &Functions::new(), limits)
}

/// Evaluate a parsed expression with an explicit pure-function namespace.
/// Nested calls share the same limits and cannot access the caller's locals.
pub fn evaluate_with_functions(
    expression: &Expr,
    environment: &Environment,
    functions: &Functions,
    limits: Limits,
) -> Result<CanonicalValue, EvaluationError> {
    validate_limits(limits)?;
    let mut context = Context {
        limits,
        steps: 0,
        functions,
        aliases: None,
        session_functions: None,
        repl_bindings: false,
        restrict_function_names: false,
        reject_unhandled_field_calls: false,
        effects: None,
        namespace: None,
        transfer: None,
    };
    context.items(functions.len())?;
    let mut scope = Scope::from_environment(environment, &mut context)?;
    let value = context.evaluate(expression, &mut scope, 0)?;
    if context.transfer.is_some() {
        return Err(error("ORNA-EVAL-UNSUPPORTED"));
    }
    value.canonical()
}

/// Invoke a parsed, statically checked pure function with canonical named
/// arguments. Defaults execute in declaration order after earlier parameters
/// are bound, and only when omitted. Argument admission, defaults, and the body
/// share one resource budget. This does not provide module or external calls.
pub fn evaluate_function(
    parameters: &[Parameter],
    body: &Expr,
    environment: &Environment,
    arguments: &Environment,
    limits: Limits,
) -> Result<CanonicalValue, EvaluationError> {
    validate_limits(limits)?;
    let functions = Functions::new();
    let mut context = Context {
        limits,
        steps: 0,
        functions: &functions,
        aliases: None,
        session_functions: None,
        repl_bindings: false,
        restrict_function_names: false,
        reject_unhandled_field_calls: false,
        effects: None,
        namespace: None,
        transfer: None,
    };
    let supplied = supplied_arguments(arguments, &mut context)?;
    let captured = Scope::from_environment(environment, &mut context)?;
    invoke_pure(&mut context, parameters, body, captured, supplied, 0).and_then(Value::canonical)
}

/// Invoke an admitted named function with canonical host-provided arguments.
/// Its body and defaults can call other admitted functions within one budget.
pub fn invoke_named(
    name: &str,
    functions: &Functions,
    arguments: &Environment,
    limits: Limits,
) -> Result<CanonicalValue, EvaluationError> {
    validate_limits(limits)?;
    let mut context = Context {
        limits,
        steps: 0,
        functions,
        aliases: None,
        session_functions: None,
        repl_bindings: false,
        restrict_function_names: false,
        reject_unhandled_field_calls: false,
        effects: None,
        namespace: function_namespace(name),
        transfer: None,
    };
    context.items(functions.len())?;
    let function = functions.get(name).ok_or_else(|| error("ORNA-EVAL-NAME"))?;
    let supplied = supplied_arguments(arguments, &mut context)?;
    let captured = Scope::from_environment(&function.environment, &mut context)?;
    invoke_pure(
        &mut context,
        &function.parameters,
        &function.body,
        captured,
        supplied,
        0,
    )
    .and_then(Value::canonical)
}

/// Invokes an admitted function with one effect handler. Nested pure calls and
/// handled effects share the same evaluator resource context.
pub fn invoke_named_with_effects(
    name: &str,
    functions: &Functions,
    arguments: &Environment,
    limits: Limits,
    effects: &mut dyn EffectHandler,
) -> Result<CanonicalValue, EvaluationError> {
    validate_limits(limits)?;
    let mut context = Context {
        limits,
        steps: 0,
        functions,
        aliases: None,
        session_functions: None,
        repl_bindings: false,
        restrict_function_names: false,
        reject_unhandled_field_calls: false,
        effects: Some(effects),
        namespace: function_namespace(name),
        transfer: None,
    };
    context.items(functions.len())?;
    let function = functions.get(name).ok_or_else(|| error("ORNA-EVAL-NAME"))?;
    let supplied = supplied_arguments(arguments, &mut context)?;
    let captured = Scope::from_environment(&function.environment, &mut context)?;
    invoke_pure(
        &mut context,
        &function.parameters,
        &function.body,
        captured,
        supplied,
        0,
    )
    .and_then(Value::canonical)
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum ArgumentKey {
    Name(String),
    Position(usize),
}

fn parameter_key(parameter: &Parameter, index: usize) -> ArgumentKey {
    match &parameter.pattern {
        Pattern::Name(name, _) if name != "_" => ArgumentKey::Name(name.clone()),
        _ => ArgumentKey::Position(index),
    }
}

fn supplied_arguments(
    arguments: &Environment,
    context: &mut Context,
) -> Result<BTreeMap<ArgumentKey, Value>, EvaluationError> {
    Ok(Scope::from_environment(arguments, context)?
        .0
        .into_iter()
        .map(|(name, value)| (ArgumentKey::Name(name), value))
        .collect())
}

fn invoke_pure(
    context: &mut Context,
    parameters: &[Parameter],
    body: &Expr,
    mut scope: Scope,
    supplied: BTreeMap<ArgumentKey, Value>,
    depth: usize,
) -> Result<Value, EvaluationError> {
    context.depth(depth)?;
    context.items(parameters.len())?;
    context.items(supplied.len())?;
    let mut keys = BTreeSet::new();
    let mut bound_names = BTreeSet::new();
    for (index, parameter) in parameters.iter().enumerate() {
        let key = parameter_key(parameter, index);
        for name in pattern_names(&parameter.pattern) {
            if name == "_" {
                continue;
            }
            if name.len() > context.limits.max_string_bytes {
                return Err(error("ORNA-EVAL-LIMIT"));
            }
            if !bound_names.insert(name) {
                return Err(error("ORNA-EVAL-ARGUMENT"));
            }
        }
        if !keys.insert(key.clone())
            || (!supplied.contains_key(&key) && parameter.default.is_none())
        {
            return Err(error("ORNA-EVAL-ARGUMENT"));
        }
    }
    if supplied.keys().any(|key| !keys.contains(key)) {
        return Err(error("ORNA-EVAL-ARGUMENT"));
    }
    for (index, parameter) in parameters.iter().enumerate() {
        let value = if let Some(value) = supplied.get(&parameter_key(parameter, index)) {
            value.clone()
        } else {
            context.evaluate(
                parameter
                    .default
                    .as_ref()
                    .expect("omitted defaults were admitted above"),
                &mut scope,
                depth,
            )?
        };
        if !bind(&parameter.pattern, value, &mut scope, context, depth + 1)? {
            return Err(error("ORNA-EVAL-TYPE"));
        }
        context.items(scope.0.len())?;
    }
    let value = context.evaluate(body, &mut scope, depth)?;
    match context.transfer.take() {
        Some(Transfer::Return(value)) => Ok(value),
        // A called function/lambda is a transfer boundary. A loop transfer
        // cannot target a loop outside that boundary.
        Some(Transfer::Break(_) | Transfer::Continue) => Err(error("ORNA-EVAL-UNSUPPORTED")),
        None => Ok(value),
    }
}

fn check_limits(source: &str, limits: Limits) -> Result<(), EvaluationError> {
    validate_limits(limits)?;
    if source.len() > limits.max_source_bytes {
        Err(error("ORNA-EVAL-LIMIT"))
    } else {
        Ok(())
    }
}
fn validate_limits(limits: Limits) -> Result<(), EvaluationError> {
    if limits.max_source_bytes == 0
        || limits.max_steps == 0
        || limits.max_depth == 0
        || limits.max_collection_items == 0
        || limits.max_string_bytes == 0
        || limits.max_integer_digits == 0
    {
        Err(error("ORNA-EVAL-LIMIT"))
    } else {
        Ok(())
    }
}
fn error(code: &'static str) -> EvaluationError {
    // No parser diagnostic, source text, span, input value, or filesystem data
    // crosses this boundary.
    EvaluationError::redacted(SafeText::new(code).expect("static safe code"))
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Value {
    Null,
    Unit,
    Bool(bool),
    Int(BigInt),
    Decimal(DecimalValue),
    Float(u64),
    String(String),
    Range {
        lower: Option<BigInt>,
        upper: Option<BigInt>,
        upper_inclusive: bool,
    },
    List(Vec<Value>),
    Tuple(Vec<Value>),
    Record(BTreeMap<String, Value>),
    NominalRecord {
        type_id: Raw,
        fields: Vec<(Raw, Value)>,
    },
    Enum {
        type_id: Raw,
        variant_id: Raw,
        payload: Option<Box<Value>>,
    },
    Option(Option<Box<Value>>),
    Function(String),
    Closure(Rc<Closure>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Closure {
    parameters: Vec<Parameter>,
    body: Expr,
    captured: Scope,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DecimalValue {
    coefficient: BigInt,
    exponent10: BigInt,
}
impl DecimalValue {
    fn new(mut coefficient: BigInt, mut exponent10: BigInt) -> Result<Self, EvaluationError> {
        if coefficient.is_zero() {
            return Ok(Self {
                coefficient,
                exponent10: BigInt::zero(),
            });
        }
        while (&coefficient % 10u8).is_zero() {
            coefficient /= 10u8;
            exponent10 += 1;
        }
        if exponent10
            .abs()
            .to_usize()
            .is_none_or(|value| value > DEFAULT_INTEGER_DIGITS)
        {
            return Err(error("ORNA-EVAL-LIMIT"));
        }
        Ok(Self {
            coefficient,
            exponent10,
        })
    }
    fn add(&self, other: &Self) -> Result<Self, EvaluationError> {
        let exponent = self.exponent10.clone().min(other.exponent10.clone());
        let left = (&self.exponent10 - &exponent)
            .to_usize()
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        let right = (&other.exponent10 - &exponent)
            .to_usize()
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        if left > DEFAULT_INTEGER_DIGITS || right > DEFAULT_INTEGER_DIGITS {
            return Err(error("ORNA-EVAL-LIMIT"));
        }
        Self::new(
            &self.coefficient * BigInt::from(10u8).pow(left as u32)
                + &other.coefficient * BigInt::from(10u8).pow(right as u32),
            exponent,
        )
    }
    fn multiply(&self, other: &Self) -> Result<Self, EvaluationError> {
        Self::new(
            &self.coefficient * &other.coefficient,
            &self.exponent10 + &other.exponent10,
        )
    }
    fn divide(&self, other: &Self) -> Result<Self, EvaluationError> {
        if other.coefficient.is_zero() {
            return Err(error("ORNA-EVAL-DIVIDE-BY-ZERO"));
        }
        let gcd = self.coefficient.gcd(&other.coefficient);
        let mut numerator = &self.coefficient / &gcd;
        let mut denominator = (&other.coefficient / gcd).abs();
        if other.coefficient.sign() == Sign::Minus {
            numerator = -numerator;
        }
        let mut twos = 0usize;
        let mut fives = 0usize;
        while (&denominator % 2u8).is_zero() {
            denominator /= 2u8;
            twos += 1;
        }
        while (&denominator % 5u8).is_zero() {
            denominator /= 5u8;
            fives += 1;
        }
        if denominator != BigInt::from(1) {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        let scale = twos.max(fives);
        if scale > DEFAULT_INTEGER_DIGITS {
            return Err(error("ORNA-EVAL-LIMIT"));
        }
        numerator *= BigInt::from(2u8).pow((scale - twos) as u32);
        numerator *= BigInt::from(5u8).pow((scale - fives) as u32);
        Self::new(
            numerator,
            &self.exponent10 - &other.exponent10 - BigInt::from(scale),
        )
    }
}

impl Value {
    fn contains_callable(&self) -> bool {
        match self {
            Self::Function(_) | Self::Closure(_) => true,
            Self::List(values) | Self::Tuple(values) => values.iter().any(Self::contains_callable),
            Self::Record(values) => values.values().any(Self::contains_callable),
            Self::NominalRecord { fields, .. } => {
                fields.iter().any(|(_, value)| value.contains_callable())
            }
            Self::Enum { payload, .. } | Self::Option(payload) => payload
                .as_ref()
                .is_some_and(|value| value.contains_callable()),
            _ => false,
        }
    }
    fn contains_float(&self) -> bool {
        match self {
            Self::Float(_) => true,
            Self::List(values) | Self::Tuple(values) => values.iter().any(Self::contains_float),
            Self::Record(values) => values.values().any(Self::contains_float),
            Self::NominalRecord { fields, .. } => {
                fields.iter().any(|(_, value)| value.contains_float())
            }
            Self::Enum { payload, .. } | Self::Option(payload) => {
                payload.as_ref().is_some_and(|value| value.contains_float())
            }
            _ => false,
        }
    }
    fn canonical(self) -> Result<CanonicalValue, EvaluationError> {
        CanonicalValue::new(self.raw()?).map_err(|_| error("ORNA-EVAL-VALUE"))
    }
    fn raw(self) -> Result<Raw, EvaluationError> {
        Ok(match self {
            Self::Function(_) | Self::Closure(_) => return Err(error("ORNA-EVAL-UNSUPPORTED")),
            Self::Null => Raw::Null,
            Self::Unit => Raw::Tag(60014, Box::new(Raw::Array(vec![]))),
            Self::Bool(value) => Raw::Bool(value),
            Self::Int(value) => Raw::Int(value),
            Self::Decimal(value) => Raw::Tag(
                60000,
                Box::new(Raw::Array(vec![
                    Raw::Int(value.coefficient),
                    Raw::Int(value.exponent10),
                ])),
            ),
            Self::Float(bits) => Raw::Float(bits),
            Self::String(value) => Raw::Text(value),
            Self::Range {
                lower,
                upper,
                upper_inclusive,
            } => Raw::Tag(
                60019,
                Box::new(Raw::Array(vec![
                    raw_option_int(lower),
                    raw_option_int(upper),
                    Raw::Bool(upper_inclusive),
                ])),
            ),
            Self::List(values) | Self::Tuple(values) => Raw::Array(
                values
                    .into_iter()
                    .map(Value::raw)
                    .collect::<Result<_, _>>()?,
            ),
            Self::Record(values) => {
                // OVB map keys are ordered by their canonical text encoding:
                // text length first, then bytewise lexical order.
                let mut fields = values.into_iter().collect::<Vec<_>>();
                fields.sort_by(|(left, _), (right, _)| {
                    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
                });
                Raw::Map(
                    fields
                        .into_iter()
                        .map(|(key, value)| value.raw().map(|value| (Raw::Text(key), value)))
                        .collect::<Result<_, _>>()?,
                )
            }
            Self::NominalRecord { type_id, fields } => Raw::Tag(
                60009,
                Box::new(Raw::Array(vec![
                    type_id,
                    Raw::Array(
                        fields
                            .into_iter()
                            .map(|(key, value)| {
                                value.raw().map(|value| Raw::Array(vec![key, value]))
                            })
                            .collect::<Result<_, _>>()?,
                    ),
                ])),
            ),
            Self::Enum {
                type_id,
                variant_id,
                payload,
            } => Raw::Tag(
                60008,
                Box::new(Raw::Array(vec![
                    type_id,
                    variant_id,
                    payload
                        .map(|value| value.raw())
                        .transpose()?
                        .unwrap_or(Raw::Null),
                ])),
            ),
            Self::Option(value) => Raw::Tag(
                60013,
                Box::new(match value {
                    Some(value) => Raw::Array(vec![Raw::Int(1.into()), value.raw()?]),
                    None => Raw::Array(vec![Raw::Int(0.into())]),
                }),
            ),
        })
    }
    fn from_canonical(
        value: &CanonicalValue,
        context: &mut Context,
        depth: usize,
    ) -> Result<Self, EvaluationError> {
        context.depth(depth)?;
        match value.raw() {
            Raw::Null => Ok(Self::Null),
            Raw::Tag(60014, boxed) if matches!(boxed.as_ref(), Raw::Array(values) if values.is_empty()) => {
                Ok(Self::Unit)
            }
            Raw::Bool(value) => Ok(Self::Bool(*value)),
            Raw::Int(value) => context.integer(value.clone()).map(Self::Int),
            Raw::Float(bits)
                if f64::from_bits(*bits).is_finite() || *bits == CANONICAL_NAN_BITS =>
            {
                Ok(Self::Float(*bits))
            }
            Raw::Text(value) => context.string(value.clone()).map(Self::String),
            Raw::Array(values) => {
                context.items(values.len())?;
                values
                    .iter()
                    .map(|value| Self::from_raw(value, context, depth + 1))
                    .collect::<Result<Vec<_>, _>>()
                    .map(Self::List)
            }
            Raw::Map(values) => {
                context.items(values.len())?;
                let mut record = BTreeMap::new();
                for (key, value) in values {
                    let Raw::Text(key) = key else {
                        return Err(error("ORNA-EVAL-UNSUPPORTED"));
                    };
                    context.string(key.clone())?;
                    if record
                        .insert(key.clone(), Self::from_raw(value, context, depth + 1)?)
                        .is_some()
                    {
                        return Err(error("ORNA-EVAL-VALUE"));
                    }
                }
                Ok(Self::Record(record))
            }
            Raw::Tag(60000, boxed) => Self::decimal_from_raw(boxed, context),
            Raw::Tag(60008, boxed) => Self::enum_from_raw(boxed, context, depth),
            Raw::Tag(60009, boxed) => Self::nominal_record_from_raw(boxed, context, depth),
            Raw::Tag(60013, boxed) => Self::option_from_raw(boxed, context, depth),
            Raw::Tag(60019, boxed) => Self::range_from_raw(boxed, context, depth),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn from_raw(raw: &Raw, context: &mut Context, depth: usize) -> Result<Self, EvaluationError> {
        let value = CanonicalValue::new(raw.clone()).map_err(|_| error("ORNA-EVAL-VALUE"))?;
        Self::from_canonical(&value, context, depth)
    }
    fn decimal_from_raw(raw: &Raw, context: &mut Context) -> Result<Self, EvaluationError> {
        let Raw::Array(parts) = raw else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        let [Raw::Int(coefficient), Raw::Int(exponent)] = parts.as_slice() else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        context.integer(coefficient.clone())?;
        context.integer(exponent.clone())?;
        DecimalValue::new(coefficient.clone(), exponent.clone()).map(Self::Decimal)
    }
    fn enum_from_raw(
        raw: &Raw,
        context: &mut Context,
        depth: usize,
    ) -> Result<Self, EvaluationError> {
        let Raw::Array(parts) = raw else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        let [type_id, variant_id, payload] = parts.as_slice() else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        let payload = match payload {
            Raw::Null => None,
            value => Some(Box::new(Self::from_raw(value, context, depth + 1)?)),
        };
        Ok(Self::Enum {
            type_id: type_id.clone(),
            variant_id: variant_id.clone(),
            payload,
        })
    }
    fn nominal_record_from_raw(
        raw: &Raw,
        context: &mut Context,
        depth: usize,
    ) -> Result<Self, EvaluationError> {
        let Raw::Array(parts) = raw else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        let [type_id, Raw::Array(raw_fields)] = parts.as_slice() else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        context.items(raw_fields.len())?;
        let mut fields = Vec::with_capacity(raw_fields.len());
        for field in raw_fields {
            let Raw::Array(parts) = field else {
                return Err(error("ORNA-EVAL-VALUE"));
            };
            let [key, value] = parts.as_slice() else {
                return Err(error("ORNA-EVAL-VALUE"));
            };
            if let Raw::Text(name) = key {
                context.string(name.clone())?;
            }
            fields.push((key.clone(), Self::from_raw(value, context, depth + 1)?));
        }
        Ok(Self::NominalRecord {
            type_id: type_id.clone(),
            fields,
        })
    }
    fn option_from_raw(
        raw: &Raw,
        context: &mut Context,
        depth: usize,
    ) -> Result<Self, EvaluationError> {
        let Raw::Array(parts) = raw else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        match parts.as_slice() {
            [Raw::Int(tag)] if tag.is_zero() => Ok(Self::Option(None)),
            [Raw::Int(tag), value] if *tag == BigInt::from(1) => Ok(Self::Option(Some(Box::new(
                Self::from_raw(value, context, depth + 1)?,
            )))),
            _ => Err(error("ORNA-EVAL-VALUE")),
        }
    }
    fn range_from_raw(
        raw: &Raw,
        context: &mut Context,
        depth: usize,
    ) -> Result<Self, EvaluationError> {
        let Raw::Array(parts) = raw else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        let [lower, upper, Raw::Bool(upper_inclusive)] = parts.as_slice() else {
            return Err(error("ORNA-EVAL-VALUE"));
        };
        let lower = option_int(lower, context, depth + 1)?;
        let upper = option_int(upper, context, depth + 1)?;
        Ok(Self::Range {
            lower,
            upper,
            upper_inclusive: *upper_inclusive,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Scope(BTreeMap<String, Value>, BTreeSet<String>, BTreeSet<String>);
impl Scope {
    fn from_environment(
        environment: &Environment,
        context: &mut Context,
    ) -> Result<Self, EvaluationError> {
        context.items(environment.len())?;
        let mut values = BTreeMap::new();
        for (name, value) in environment {
            if name.len() > context.limits.max_string_bytes {
                return Err(error("ORNA-EVAL-LIMIT"));
            }
            values.insert(name.clone(), Value::from_canonical(value, context, 0)?);
        }
        Ok(Self(values, BTreeSet::new(), BTreeSet::new()))
    }
}

struct Context<'functions, 'effects> {
    limits: Limits,
    steps: u64,
    functions: &'functions Functions,
    aliases: Option<&'functions BTreeMap<String, String>>,
    session_functions: Option<&'functions BTreeSet<String>>,
    repl_bindings: bool,
    restrict_function_names: bool,
    reject_unhandled_field_calls: bool,
    effects: Option<&'effects mut dyn EffectHandler>,
    namespace: Option<String>,
    transfer: Option<Transfer>,
}

/// An evaluator-only non-local control transfer. It never crosses the public
/// canonical-value boundary: a function consumes `Return`, and a finite `for`
/// consumes `Break` or `Continue`.
enum Transfer {
    Return(Value),
    Break(Value),
    Continue,
}
impl Context<'_, '_> {
    fn step(&mut self) -> Result<(), EvaluationError> {
        self.steps = self
            .steps
            .checked_add(1)
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        if self.steps > self.limits.max_steps {
            Err(error("ORNA-EVAL-LIMIT"))
        } else {
            Ok(())
        }
    }
    fn depth(&self, depth: usize) -> Result<(), EvaluationError> {
        if depth > self.limits.max_depth {
            Err(error("ORNA-EVAL-LIMIT"))
        } else {
            Ok(())
        }
    }
    fn items(&self, count: usize) -> Result<(), EvaluationError> {
        if count > self.limits.max_collection_items {
            Err(error("ORNA-EVAL-LIMIT"))
        } else {
            Ok(())
        }
    }
    fn string(&self, value: String) -> Result<String, EvaluationError> {
        if value.len() > self.limits.max_string_bytes {
            Err(error("ORNA-EVAL-LIMIT"))
        } else {
            Ok(value)
        }
    }
    fn integer(&self, value: BigInt) -> Result<BigInt, EvaluationError> {
        if value.to_str_radix(10).len() > self.limits.max_integer_digits {
            Err(error("ORNA-EVAL-LIMIT"))
        } else {
            Ok(value)
        }
    }
    fn evaluate(
        &mut self,
        expression: &Expr,
        scope: &mut Scope,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        // Once a block emits a transfer, every enclosing expression is only a
        // transport frame. Do not run siblings, effects, or later statements.
        if self.transfer.is_some() {
            return Ok(Value::Null);
        }
        self.step()?;
        self.depth(depth)?;
        let result = match expression {
            Expr::Name { text, .. } => scope
                .0
                .get(text)
                .cloned()
                .or_else(|| {
                    (!self.restrict_function_names && self.functions.contains_key(text))
                        .then(|| Value::Function(text.clone()))
                })
                .or_else(|| {
                    self.aliases
                        .and_then(|aliases| aliases.get(text))
                        .filter(|name| self.functions.contains_key(*name))
                        .cloned()
                        .map(Value::Function)
                })
                .or_else(|| {
                    let namespace = self.namespace.as_deref()?;
                    let qualified = format!("{namespace}.{text}");
                    self.functions
                        .contains_key(&qualified)
                        .then_some(Value::Function(qualified))
                })
                .ok_or_else(|| error("ORNA-EVAL-NAME")),
            Expr::Lambda {
                parameters, body, ..
            } => {
                self.items(parameters.len())?;
                self.items(scope.0.len())?;
                let mut captured = scope.clone();
                captured.1.extend(captured.0.keys().cloned());
                Ok(Value::Closure(Rc::new(Closure {
                    parameters: parameters
                        .iter()
                        .map(|parameter| Parameter {
                            pattern: parameter.pattern.clone(),
                            annotation: parameter.annotation.clone(),
                            default: None,
                            span: parameter.span.clone(),
                        })
                        .collect(),
                    body: body.as_ref().clone(),
                    captured,
                })))
            }
            Expr::Literal { text, kind, .. } => self.literal(text, *kind),
            Expr::InterpolatedString { segments, .. } => {
                self.interpolated_string(segments, scope, depth)
            }
            Expr::Group { inner, .. } => self.evaluate(inner, scope, depth + 1),
            Expr::Unary { op, rhs, .. } => {
                let value = self.evaluate(rhs, scope, depth + 1)?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                self.unary(op, value)
            }
            Expr::Range {
                lower,
                operator,
                upper,
                ..
            } => {
                let lower = lower
                    .as_deref()
                    .map(|value| self.evaluate(value, scope, depth + 1))
                    .transpose()?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                let upper = upper
                    .as_deref()
                    .map(|value| self.evaluate(value, scope, depth + 1))
                    .transpose()?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                self.integer_range_optional(lower, upper, operator == "..=")
            }
            Expr::Binary { lhs, op, rhs, .. } => self.binary(op, lhs, rhs, scope, depth),
            Expr::List { elements, .. } => self.sequence(elements, scope, depth).map(Value::List),
            Expr::Tuple { elements, .. } => self.sequence(elements, scope, depth).map(Value::Tuple),
            Expr::Record { fields, .. } => {
                self.items(fields.len())?;
                let mut result = BTreeMap::new();
                for field in fields {
                    let value = self.evaluate(&field.value, scope, depth + 1)?;
                    if self.transfer.is_some() {
                        return Ok(Value::Null);
                    }
                    if result.insert(field.name.clone(), value).is_some() {
                        return Err(error("ORNA-EVAL-VALUE"));
                    }
                }
                Ok(Value::Record(result))
            }
            Expr::Block {
                statements, tail, ..
            } => self.block(statements, tail.as_deref(), scope, depth),
            Expr::Call {
                callee, arguments, ..
            } => self.call(callee, arguments, None, scope, depth),
            Expr::Index { base, index, .. } => {
                let base = self.evaluate(base, scope, depth + 1)?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                let index = self.evaluate(index, scope, depth + 1)?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                self.index(base, index)
            }
            Expr::Field { base, name, .. } => {
                let Value::Record(fields) = self.evaluate(base, scope, depth + 1)? else {
                    if self.transfer.is_some() {
                        return Ok(Value::Null);
                    }
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                fields
                    .get(name)
                    .cloned()
                    .ok_or_else(|| error("ORNA-EVAL-FIELD"))
            }
            Expr::Control {
                kind: ControlKind::If,
                condition: Some(condition),
                body: Some(body),
                alternate,
                ..
            } => match self.evaluate(condition, scope, depth + 1)? {
                _ if self.transfer.is_some() => Ok(Value::Null),
                Value::Bool(true) => self.evaluate(body, scope, depth + 1),
                Value::Bool(false) => alternate.as_deref().map_or(Ok(Value::Null), |value| {
                    self.evaluate(value, scope, depth + 1)
                }),
                _ => Err(error("ORNA-EVAL-TYPE")),
            },
            Expr::Control {
                kind: ControlKind::Case,
                condition: Some(condition),
                arms,
                ..
            } => self.case(condition, arms, scope, depth),
            Expr::Control {
                kind: ControlKind::For,
                binding: Some(binding),
                condition: Some(iterable),
                body: Some(body),
                arms,
                alternate: None,
                ..
            } => {
                if arms.len() != 1
                    || arms[0].guard.is_some()
                    || arms[0].pattern != *binding
                    || arms[0].body != **body
                {
                    return Err(error("ORNA-EVAL-UNSUPPORTED"));
                }
                let iterable = self.evaluate(iterable, scope, depth + 1)?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                let values = self.finite_iterable(iterable)?;
                let outer_names = scope.0.keys().cloned().collect::<Vec<_>>();
                let bound_names = pattern_names(binding);
                for value in values {
                    let mut iteration = scope.clone();
                    if !bind(binding, value, &mut iteration, self, depth + 1)? {
                        return Err(error("ORNA-EVAL-TYPE"));
                    }
                    self.evaluate(body, &mut iteration, depth + 1)?;
                    match self.transfer.take() {
                        Some(Transfer::Continue) => {}
                        Some(Transfer::Break(value)) => {
                            if !matches!(value, Value::Null) {
                                return Err(error("ORNA-EVAL-UNSUPPORTED"));
                            }
                            for name in &outer_names {
                                if bound_names.contains(name) {
                                    continue;
                                }
                                if let Some(value) = iteration.0.get(name).cloned() {
                                    scope.0.insert(name.clone(), value);
                                }
                            }
                            return Ok(Value::Null);
                        }
                        Some(transfer @ Transfer::Return(_)) => {
                            self.transfer = Some(transfer);
                            return Ok(Value::Null);
                        }
                        None => {}
                    }
                    for name in &outer_names {
                        if bound_names.contains(name) {
                            continue;
                        }
                        if let Some(value) = iteration.0.get(name).cloned() {
                            scope.0.insert(name.clone(), value);
                        }
                    }
                }
                Ok(Value::Null)
            }
            Expr::ReplBinding { text, .. } if self.repl_bindings => scope
                .0
                .get(text)
                .cloned()
                .ok_or_else(|| error("ORNA-EVAL-NAME")),
            Expr::ReplBinding { .. } => Err(error("ORNA-EVAL-UNSUPPORTED")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        };
        if self.transfer.is_some() {
            Ok(Value::Null)
        } else {
            result
        }
    }
    fn literal(&self, text: &str, kind: LiteralKind) -> Result<Value, EvaluationError> {
        match kind {
            LiteralKind::Null => Ok(Value::Null),
            LiteralKind::Boolean => Ok(Value::Bool(text == "true")),
            LiteralKind::String => self.string(unescape_string(text)?).map(Value::String),
            LiteralKind::Integer => parse_int(text)
                .and_then(|value| self.integer(value))
                .map(Value::Int),
            LiteralKind::Decimal => parse_decimal(text)
                .and_then(|value| {
                    self.integer(value.0)
                        .and_then(|coefficient| DecimalValue::new(coefficient, value.1))
                })
                .map(Value::Decimal),
            LiteralKind::Float => {
                let value = text
                    .strip_suffix('f')
                    .ok_or_else(|| error("ORNA-EVAL-VALUE"))?
                    .replace('_', "")
                    .parse::<f64>()
                    .map_err(|_| error("ORNA-EVAL-VALUE"))?;
                if value.is_finite() {
                    Ok(Value::Float(value.to_bits()))
                } else {
                    Err(error("ORNA-EVAL-VALUE"))
                }
            }
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn interpolated_string(
        &mut self,
        segments: &[StringSegment],
        scope: &mut Scope,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(segments.len())?;
        let mut output = String::new();
        for segment in segments {
            match segment {
                StringSegment::Text { text, .. } => output.push_str(&unescape_string_body(text)?),
                StringSegment::Expression { value, .. } => {
                    let Value::String(value) = self.evaluate(value, scope, depth + 1)? else {
                        return Err(error("ORNA-EVAL-TYPE"));
                    };
                    output.push_str(&value);
                }
            }
            if output.len() > self.limits.max_string_bytes {
                return Err(error("ORNA-EVAL-LIMIT"));
            }
        }
        Ok(Value::String(output))
    }
    fn sequence(
        &mut self,
        elements: &[Expr],
        scope: &mut Scope,
        depth: usize,
    ) -> Result<Vec<Value>, EvaluationError> {
        self.items(elements.len())?;
        let mut values = Vec::with_capacity(elements.len());
        for element in elements {
            values.push(self.evaluate(element, scope, depth + 1)?);
            if self.transfer.is_some() {
                return Ok(Vec::new());
            }
        }
        Ok(values)
    }
    fn block(
        &mut self,
        statements: &[Statement],
        tail: Option<&Expr>,
        scope: &mut Scope,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(statements.len())?;
        let outer_names = scope.0.keys().cloned().collect::<Vec<_>>();
        let mut declared_names = BTreeSet::new();
        let mut local = scope.clone();
        for statement in statements {
            match statement {
                Statement::Let { pattern, value, .. } => {
                    let value = self.evaluate(value, &mut local, depth + 1)?;
                    if !bind(pattern, value, &mut local, self, depth + 1)? {
                        return Err(error("ORNA-EVAL-TYPE"));
                    }
                    declared_names.extend(pattern_names(pattern));
                }
                Statement::Assert { value, .. } => {
                    match self.evaluate(value, &mut local, depth + 1)? {
                        Value::Bool(true) => {}
                        Value::Bool(false) => return Err(error("ORNA-EVAL-ASSERT")),
                        _ => return Err(error("ORNA-EVAL-TYPE")),
                    }
                }
                Statement::Assignment {
                    target,
                    operator,
                    value,
                    ..
                } => {
                    self.assignment(target, *operator, value, &mut local, depth + 1)?;
                }
                Statement::Expression { value, .. } => {
                    self.evaluate(value, &mut local, depth + 1)?;
                }
                Statement::Control { value, .. } => {
                    self.evaluate(value, &mut local, depth + 1)?;
                }
                Statement::Return { value, .. } => {
                    let value = value.as_ref().map_or(Ok(Value::Null), |value| {
                        self.evaluate(value, &mut local, depth + 1)
                    })?;
                    if self.transfer.is_none() {
                        self.transfer = Some(Transfer::Return(value));
                    }
                }
                Statement::Break { value, .. } => {
                    let value = value.as_ref().map_or(Ok(Value::Null), |value| {
                        self.evaluate(value, &mut local, depth + 1)
                    })?;
                    if self.transfer.is_none() {
                        self.transfer = Some(Transfer::Break(value));
                    }
                }
                Statement::Continue { .. } => {
                    self.transfer = Some(Transfer::Continue);
                }
            }
            if self.transfer.is_some() {
                break;
            }
        }
        let result = if self.transfer.is_some() {
            Value::Null
        } else {
            tail.map_or(Ok(Value::Null), |value| {
                self.evaluate(value, &mut local, depth + 1)
            })?
        };
        for name in outer_names {
            if declared_names.contains(&name) {
                continue;
            }
            if let Some(value) = local.0.get(&name).cloned() {
                scope.0.insert(name, value);
            }
        }
        Ok(result)
    }
    fn assignment(
        &mut self,
        target: &AssignmentTarget,
        operator: AssignmentOperator,
        expression: &Expr,
        scope: &mut Scope,
        depth: usize,
    ) -> Result<(), EvaluationError> {
        let AssignmentTarget::Name { name, .. } = target else {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        };
        if scope.1.contains(name) {
            return Err(error("ORNA-EVAL-IMMUTABLE-CAPTURE"));
        }
        let current = scope
            .0
            .get(name)
            .cloned()
            .ok_or_else(|| error("ORNA-EVAL-NAME"))?;
        let value = self.evaluate(expression, scope, depth + 1)?;
        if self.transfer.is_some() {
            return Ok(());
        }
        let value = match operator {
            AssignmentOperator::Set => value,
            AssignmentOperator::Add => self.apply_binary("+", current, value)?,
            AssignmentOperator::Subtract => self.apply_binary("-", current, value)?,
            AssignmentOperator::Multiply => self.apply_binary("*", current, value)?,
            AssignmentOperator::Divide => self.apply_binary("/", current, value)?,
        };
        scope.0.insert(name.clone(), value);
        Ok(())
    }
    fn unary(&self, op: &str, value: Value) -> Result<Value, EvaluationError> {
        match (op, value) {
            ("!", Value::Bool(value)) => Ok(Value::Bool(!value)),
            ("+", value @ (Value::Int(_) | Value::Decimal(_) | Value::Float(_))) => Ok(value),
            ("-", Value::Int(value)) => self.integer(-value).map(Value::Int),
            ("-", Value::Decimal(value)) => {
                DecimalValue::new(-value.coefficient, value.exponent10).map(Value::Decimal)
            }
            ("-", Value::Float(bits)) => Ok(Value::Float((-f64::from_bits(bits)).to_bits())),
            _ => Err(error("ORNA-EVAL-TYPE")),
        }
    }
    fn binary(
        &mut self,
        op: &str,
        lhs: &Expr,
        rhs: &Expr,
        scope: &mut Scope,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        match op {
            ".." | "..=" => {
                let lower = self.evaluate(lhs, scope, depth + 1)?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                let upper = self.evaluate(rhs, scope, depth + 1)?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                self.integer_range(lower, upper, op == "..=")
            }
            "in" => {
                let value = self.evaluate(lhs, scope, depth + 1)?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                let range = self.evaluate(rhs, scope, depth + 1)?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                self.range_contains(value, range)
            }
            "|" => {
                let input = self.evaluate(lhs, scope, depth + 1)?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                let (callee, arguments) = match rhs {
                    Expr::Call {
                        callee, arguments, ..
                    } => (callee.as_ref(), arguments.as_slice()),
                    _ => (rhs, &[][..]),
                };
                self.call(callee, arguments, Some(input), scope, depth)
            }
            "??" => match self.evaluate(lhs, scope, depth + 1)? {
                _ if self.transfer.is_some() => Ok(Value::Null),
                Value::Option(Some(value)) => Ok(*value),
                Value::Option(None) | Value::Null => self.evaluate(rhs, scope, depth + 1),
                _ => Err(error("ORNA-EVAL-TYPE")),
            },
            "&&" => match self.evaluate(lhs, scope, depth + 1)? {
                _ if self.transfer.is_some() => Ok(Value::Null),
                Value::Bool(false) => Ok(Value::Bool(false)),
                Value::Bool(true) => match self.evaluate(rhs, scope, depth + 1)? {
                    Value::Bool(value) => Ok(Value::Bool(value)),
                    _ => Err(error("ORNA-EVAL-TYPE")),
                },
                _ => Err(error("ORNA-EVAL-TYPE")),
            },
            "||" => match self.evaluate(lhs, scope, depth + 1)? {
                _ if self.transfer.is_some() => Ok(Value::Null),
                Value::Bool(true) => Ok(Value::Bool(true)),
                Value::Bool(false) => match self.evaluate(rhs, scope, depth + 1)? {
                    Value::Bool(value) => Ok(Value::Bool(value)),
                    _ => Err(error("ORNA-EVAL-TYPE")),
                },
                _ => Err(error("ORNA-EVAL-TYPE")),
            },
            _ => {
                let left = self.evaluate(lhs, scope, depth + 1)?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                let right = self.evaluate(rhs, scope, depth + 1)?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                self.apply_binary(op, left, right)
            }
        }
    }
    fn apply_binary(&self, op: &str, left: Value, right: Value) -> Result<Value, EvaluationError> {
        if matches!(op, "==" | "!=") {
            if left.contains_callable() || right.contains_callable() {
                return Err(error("ORNA-EVAL-UNSUPPORTED"));
            }
            let equal = match (&left, &right) {
                (Value::Float(left), Value::Float(right)) => float_ordinary_eq(*left, *right),
                _ => left == right,
            };
            return Ok(Value::Bool(if op == "==" { equal } else { !equal }));
        }
        if matches!((&left, &right), (Value::Range { .. }, Value::Range { .. })) {
            return compare(op, compare_values(&left, &right)?);
        }
        match (left, right) {
            (Value::Int(a), Value::Int(b)) => self.int_binary(op, a, b),
            (Value::Decimal(a), Value::Decimal(b)) => self.decimal_binary(op, a, b),
            (Value::Float(a), Value::Float(b)) => self.float_binary(op, a, b),
            (Value::String(a), Value::String(b)) => compare(op, a.cmp(&b)),
            (Value::Bool(a), Value::Bool(b)) => compare(op, a.cmp(&b)),
            _ => Err(error("ORNA-EVAL-TYPE")),
        }
    }
    fn integer_range(
        &self,
        lower: Value,
        upper: Value,
        upper_inclusive: bool,
    ) -> Result<Value, EvaluationError> {
        let (Value::Int(lower), Value::Int(upper)) = (lower, upper) else {
            return Err(error("ORNA-EVAL-TYPE"));
        };
        Ok(Value::Range {
            lower: Some(lower),
            upper: Some(upper),
            upper_inclusive,
        })
    }
    fn integer_range_optional(
        &self,
        lower: Option<Value>,
        upper: Option<Value>,
        upper_inclusive: bool,
    ) -> Result<Value, EvaluationError> {
        let lower = lower
            .map(|value| match value {
                Value::Int(value) => Ok(value),
                _ => Err(error("ORNA-EVAL-TYPE")),
            })
            .transpose()?;
        let upper = upper
            .map(|value| match value {
                Value::Int(value) => Ok(value),
                _ => Err(error("ORNA-EVAL-TYPE")),
            })
            .transpose()?;
        if lower.is_none() && upper.is_none() {
            return Err(error("ORNA-EVAL-TYPE"));
        }
        Ok(Value::Range {
            lower,
            upper,
            upper_inclusive,
        })
    }
    fn range_contains(&self, value: Value, range: Value) -> Result<Value, EvaluationError> {
        let (
            Value::Int(value),
            Value::Range {
                lower,
                upper,
                upper_inclusive,
            },
        ) = (value, range)
        else {
            return Err(error("ORNA-EVAL-TYPE"));
        };
        let lower_matches = lower.is_none_or(|lower| value >= lower);
        let upper_matches = upper.is_none_or(|upper| {
            if upper_inclusive {
                value <= upper
            } else {
                value < upper
            }
        });
        Ok(Value::Bool(lower_matches && upper_matches))
    }
    fn finite_iterable(&self, value: Value) -> Result<Vec<Value>, EvaluationError> {
        match value {
            Value::List(values) => {
                self.items(values.len())?;
                Ok(values)
            }
            Value::Range {
                lower: Some(lower),
                upper: Some(upper),
                upper_inclusive,
            } => self.integer_range_values(lower, upper, upper_inclusive),
            // A Range with an unbounded side is a valid Range value but not a
            // finite Iterable, so this bounded evaluator cannot consume it.
            Value::Range { .. } => Err(error("ORNA-EVAL-TYPE")),
            _ => Err(error("ORNA-EVAL-TYPE")),
        }
    }
    fn integer_range_values(
        &self,
        lower: BigInt,
        upper: BigInt,
        upper_inclusive: bool,
    ) -> Result<Vec<Value>, EvaluationError> {
        let count = if upper_inclusive {
            &upper - &lower + 1
        } else {
            &upper - &lower
        };
        if count <= BigInt::zero() {
            return Ok(Vec::new());
        }
        let count = count.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        self.items(count)?;
        let mut values = Vec::with_capacity(count);
        let mut value = lower;
        for _ in 0..count {
            values.push(Value::Int(value.clone()));
            value += 1;
        }
        Ok(values)
    }
    fn int_binary(&self, op: &str, a: BigInt, b: BigInt) -> Result<Value, EvaluationError> {
        match op {
            "+" => self.integer(a + b).map(Value::Int),
            "-" => self.integer(a - b).map(Value::Int),
            "*" => self.integer(a * b).map(Value::Int),
            "/" => {
                if b.is_zero() {
                    Err(error("ORNA-EVAL-DIVIDE-BY-ZERO"))
                } else {
                    self.integer(a / b).map(Value::Int)
                }
            }
            "%" => {
                if b.is_zero() {
                    Err(error("ORNA-EVAL-DIVIDE-BY-ZERO"))
                } else {
                    self.integer(a % b).map(Value::Int)
                }
            }
            "<" | "<=" | ">" | ">=" => compare(op, a.cmp(&b)),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn decimal_binary(
        &self,
        op: &str,
        a: DecimalValue,
        b: DecimalValue,
    ) -> Result<Value, EvaluationError> {
        match op {
            "+" => a.add(&b),
            "-" => a.add(&DecimalValue::new(-b.coefficient, b.exponent10)?),
            "*" => a.multiply(&b),
            "/" => a.divide(&b),
            _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
        .map(Value::Decimal)
    }
    fn float_binary(&self, op: &str, a: u64, b: u64) -> Result<Value, EvaluationError> {
        let (a, b) = (f64::from_bits(a), f64::from_bits(b));
        match op {
            "+" => finite_float(a + b),
            "-" => finite_float(a - b),
            "*" => finite_float(a * b),
            "/" if b == 0.0 => Err(error("ORNA-EVAL-DIVIDE-BY-ZERO")),
            "/" => finite_float(a / b),
            "%" if b == 0.0 => Err(error("ORNA-EVAL-DIVIDE-BY-ZERO")),
            "%" => finite_float(a % b),
            "<" => Ok(Value::Bool(a < b)),
            "<=" => Ok(Value::Bool(a <= b)),
            ">" => Ok(Value::Bool(a > b)),
            ">=" => Ok(Value::Bool(a >= b)),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn call(
        &mut self,
        callee: &Expr,
        arguments: &[orna_syntax_v1::Argument],
        input: Option<Value>,
        scope: &mut Scope,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        let root_collection =
            root_collection_name(callee).filter(|name| !scope.0.contains_key(*name));
        if collection_name(callee).is_some()
            && self.restrict_function_names
            && self.resolve_function_name(callee, scope).is_none()
        {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        // A verified standard-source function takes precedence over the
        // legacy bounded math fallback. This keeps admitted REPL calls on
        // ordinary import/resolution and executes their pinned source body,
        // including its declared argument names. The fallback remains only
        // for the standalone evaluator surface, which has no admitted module
        // environment.
        if math_name(callee).is_none()
            && bits_name(callee).is_none()
            && text_name(callee).is_none()
            && collection_name(callee).is_none()
            && root_collection.is_none()
            || self.resolve_function_name(callee, scope).is_some()
        {
            if matches!(callee, Expr::Field { .. }) && self.effects.is_some() {
                self.items(arguments.len() + usize::from(input.is_some()))?;
                let mut values = input.clone().into_iter().collect::<Vec<_>>();
                for argument in arguments {
                    values.push(self.evaluate(&argument.value, scope, depth + 1)?);
                    if self.transfer.is_some() {
                        return Ok(Value::Null);
                    }
                }
                let values = values
                    .into_iter()
                    .map(Value::canonical)
                    .collect::<Result<Vec<_>, _>>()?;
                let handled = self
                    .effects
                    .as_deref_mut()
                    .expect("checked effect handler")
                    .handle(callee, &values)?;
                if let Some(value) = handled {
                    return Value::from_canonical(&value, self, depth + 1);
                }
            }
            if self.reject_unhandled_field_calls
                && matches!(callee, Expr::Field { .. })
                && self.resolve_function_name(callee, scope).is_none()
            {
                return Err(error("ORNA-EVAL-UNSUPPORTED"));
            }
            // A qualified module function is a callable path, not a record
            // field lookup. Resolve it only when the complete path was
            // explicitly admitted in the function environment; ordinary
            // record fields retain their existing semantics.
            let callable = if let Some(name) = self.resolve_function_name(callee, scope) {
                Value::Function(name)
            } else {
                self.evaluate(callee, scope, depth + 1)?
            };
            if self.transfer.is_some() {
                return Ok(Value::Null);
            }
            let functions = self.functions;
            let (parameters, body, mut captured, session_owned) = match &callable {
                Value::Function(name) => {
                    let function = functions.get(name).ok_or_else(|| error("ORNA-EVAL-NAME"))?;
                    (
                        &function.parameters,
                        &function.body,
                        Scope::from_environment(&function.environment, self)?,
                        self.session_functions
                            .is_some_and(|functions| functions.contains(name)),
                    )
                }
                Value::Closure(closure) => (
                    &closure.parameters,
                    &closure.body,
                    closure.captured.clone(),
                    self.repl_bindings,
                ),
                _ => return Err(error("ORNA-EVAL-TYPE")),
            };
            captured.2.extend(
                scope
                    .2
                    .iter()
                    .filter(|name| captured.0.contains_key(*name))
                    .cloned(),
            );
            self.depth(depth + 1)?;
            self.items(arguments.len() + usize::from(input.is_some()))?;
            let mut supplied = BTreeMap::new();
            let mut positional = 0;
            if let Some(input) = input {
                let Some(parameter) = parameters.first() else {
                    return Err(error("ORNA-EVAL-ARGUMENT"));
                };
                supplied.insert(parameter_key(parameter, 0), input);
                positional = 1;
            }
            let mut named_started = false;
            for argument in arguments {
                let key = if let Some(name) = &argument.name {
                    named_started = true;
                    ArgumentKey::Name(name.clone())
                } else {
                    if named_started {
                        return Err(error("ORNA-EVAL-ARGUMENT"));
                    }
                    let Some(parameter) = parameters.get(positional) else {
                        return Err(error("ORNA-EVAL-ARGUMENT"));
                    };
                    let key = parameter_key(parameter, positional);
                    positional += 1;
                    key
                };
                if supplied.contains_key(&key) {
                    return Err(error("ORNA-EVAL-ARGUMENT"));
                }
                let value = self.evaluate(&argument.value, scope, depth + 1)?;
                if self.transfer.is_some() {
                    return Ok(Value::Null);
                }
                supplied.insert(key, value);
            }
            let previous_repl_bindings = self.repl_bindings;
            self.repl_bindings = session_owned;
            let previous_namespace = self.namespace.clone();
            self.namespace = match &callable {
                Value::Function(name) => function_namespace(name),
                Value::Closure(_) => None,
                _ => unreachable!("callable was validated above"),
            };
            let result = invoke_pure(self, parameters, body, captured, supplied, depth + 1);
            self.namespace = previous_namespace;
            self.repl_bindings = previous_repl_bindings;
            return result;
        }
        let math = math_name(callee);
        let bits = bits_name(callee);
        let text = text_name(callee);
        let collection = collection_name(callee).or(root_collection);
        let name = math
            .or(bits)
            .or(text)
            .or(collection)
            .ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?;
        let implicit = usize::from(input.is_some());
        self.items(arguments.len() + implicit)?;
        let mut values = input.into_iter().collect::<Vec<_>>();
        let mut explicit = Vec::with_capacity(arguments.len());
        for argument in arguments {
            explicit.push(self.evaluate(&argument.value, scope, depth + 1)?);
            if self.transfer.is_some() {
                return Ok(Value::Null);
            }
        }
        values.extend(explicit);
        let values = named_arguments(name, arguments, values, implicit, collection.is_some())?;
        if math.is_some() {
            self.math(name, values)
        } else if bits.is_some() {
            self.bits(name, values)
        } else if text.is_some() {
            self.text(name, values)
        } else {
            self.collection(name, values, depth)
        }
    }
    fn resolve_function_name(&self, expression: &Expr, scope: &Scope) -> Option<String> {
        let name = function_name(expression)?;
        if let Some(root) = function_root_name(expression)
            && scope.0.contains_key(root)
            && !scope.2.contains(root)
        {
            return None;
        }
        if let Some(namespace) = self.namespace.as_deref() {
            if !name.contains('.') {
                let qualified = format!("{namespace}.{name}");
                if self.functions.contains_key(&qualified) {
                    return Some(qualified);
                }
            } else if self.functions.contains_key(&name) {
                return Some(name);
            }
        }
        if let Some(alias) = self.aliases.and_then(|aliases| aliases.get(&name))
            && self.functions.contains_key(alias)
        {
            return Some(alias.clone());
        }
        if !self.restrict_function_names && self.functions.contains_key(&name) {
            return Some(name);
        }
        if name.contains('.') {
            return None;
        }
        let namespace = self.namespace.as_deref()?;
        let qualified = format!("{namespace}.{name}");
        self.functions.contains_key(&qualified).then_some(qualified)
    }
    fn index(&self, base: Value, index: Value) -> Result<Value, EvaluationError> {
        let Value::Int(index) = index else {
            return Err(error("ORNA-EVAL-TYPE"));
        };
        let index = index.to_usize().ok_or_else(|| error("ORNA-EVAL-INDEX"))?;
        match base {
            Value::List(values) | Value::Tuple(values) => values
                .get(index)
                .cloned()
                .ok_or_else(|| error("ORNA-EVAL-INDEX")),
            _ => Err(error("ORNA-EVAL-TYPE")),
        }
    }
    fn case(
        &mut self,
        condition: &Expr,
        arms: &[orna_syntax_v1::CaseArm],
        scope: &mut Scope,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        let value = self.evaluate(condition, scope, depth + 1)?;
        if self.transfer.is_some() {
            return Ok(Value::Null);
        }
        self.items(arms.len())?;
        for arm in arms {
            let mut local = scope.clone();
            if !bind(&arm.pattern, value.clone(), &mut local, self, depth + 1)? {
                continue;
            }
            if let Some(guard) = &arm.guard {
                match self.evaluate(guard, &mut local, depth + 1)? {
                    _ if self.transfer.is_some() => return Ok(Value::Null),
                    Value::Bool(true) => {}
                    Value::Bool(false) => continue,
                    _ => return Err(error("ORNA-EVAL-TYPE")),
                }
            }
            return self.evaluate(&arm.body, &mut local, depth + 1);
        }
        Err(error("ORNA-EVAL-NO-MATCH"))
    }
    fn math(&self, name: &str, values: Vec<Value>) -> Result<Value, EvaluationError> {
        match (name, values.as_slice()) {
            ("increment", [value]) => self.apply_binary("+", value.clone(), one_like(value)?),
            ("decrement", [value]) => self.apply_binary("-", value.clone(), one_like(value)?),
            ("is_zero", [Value::Int(value)]) => Ok(Value::Bool(value.is_zero())),
            ("is_zero", [Value::Decimal(value)]) => Ok(Value::Bool(value.coefficient.is_zero())),
            ("is_zero", [Value::Float(value)]) => Ok(Value::Bool(f64::from_bits(*value) == 0.0)),
            ("min", [a, b]) => ordered(a, b, true),
            ("max", [a, b]) => ordered(a, b, false),
            ("clamp", [value, low, high]) => {
                if compare_values(low, high)? == std::cmp::Ordering::Greater {
                    return Err(error("ORNA-EVAL-VALUE"));
                }
                if compare_values(value, low)? == std::cmp::Ordering::Less {
                    Ok(low.clone())
                } else if compare_values(value, high)? == std::cmp::Ordering::Greater {
                    Ok(high.clone())
                } else {
                    Ok(value.clone())
                }
            }
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn bits(&self, name: &str, values: Vec<Value>) -> Result<Value, EvaluationError> {
        match (name, values.as_slice()) {
            ("bit_or", [Value::Int(left), Value::Int(right)]) => {
                Ok(Value::Int(self.integer(left | right)?))
            }
            ("bit_and", [Value::Int(left), Value::Int(right)]) => {
                Ok(Value::Int(self.integer(left & right)?))
            }
            ("bit_xor", [Value::Int(left), Value::Int(right)]) => {
                Ok(Value::Int(self.integer(left ^ right)?))
            }
            ("bit_not", [Value::Int(value)]) => {
                Ok(Value::Int(self.integer(-value - BigInt::from(1))?))
            }
            ("shift_left", [Value::Int(value), Value::Int(count)]) => self.shift_left(value, count),
            ("shift_right", [Value::Int(value), Value::Int(count)]) => {
                self.shift_right(value, count)
            }
            ("bit_or" | "bit_and" | "bit_xor" | "bit_not" | "shift_left" | "shift_right", _)
                if values.iter().any(|value| !matches!(value, Value::Int(_))) =>
            {
                Err(error("ORNA-EVAL-TYPE"))
            }
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn text(&self, name: &str, values: Vec<Value>) -> Result<Value, EvaluationError> {
        match (name, values.as_slice()) {
            ("trim", [Value::String(value)]) => {
                self.string(value.trim().to_owned()).map(Value::String)
            }
            ("split", [Value::String(value), Value::String(separator)]) => {
                let count = if separator.is_empty() {
                    value.chars().count()
                } else {
                    value.split(separator).count()
                };
                self.items(count)?;
                let fields = if separator.is_empty() {
                    value
                        .chars()
                        .map(|scalar| Value::String(scalar.to_string()))
                        .collect()
                } else {
                    value
                        .split(separator)
                        .map(|field| Value::String(field.to_owned()))
                        .collect()
                };
                Ok(Value::List(fields))
            }
            ("join", [Value::List(values), Value::String(separator)]) => {
                self.items(values.len())?;
                let mut output = String::new();
                for (index, value) in values.iter().enumerate() {
                    let Value::String(value) = value else {
                        return Err(error("ORNA-EVAL-TYPE"));
                    };
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
                self.string(value.replace(from, to)).map(Value::String)
            }
            ("normalise", [Value::String(value), Value::String(form)]) => match form.as_str() {
                "NFC" => self.string(value.nfc().collect()).map(Value::String),
                "NFD" => self.string(value.nfd().collect()).map(Value::String),
                _ => Err(error("ORNA-EVAL-VALUE")),
            },
            ("lower", [Value::String(value)]) => {
                self.string(value.to_lowercase()).map(Value::String)
            }
            ("upper", [Value::String(value)]) => {
                self.string(value.to_uppercase()).map(Value::String)
            }
            ("trim" | "lower" | "upper", [_])
            | ("split" | "starts_with" | "ends_with" | "contains", [_, _])
            | ("join", [_, _])
            | ("replace", [_, _, _])
            | ("normalise", [_, _]) => Err(error("ORNA-EVAL-TYPE")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn collection(
        &mut self,
        name: &str,
        values: Vec<Value>,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        match (name, values.as_slice()) {
            ("chunk", [Value::List(values), Value::Int(size)]) => {
                self.items(values.len())?;
                let size = self.positive_collection_size(size)?;
                let mut chunks = Vec::new();
                for chunk in values.chunks(size) {
                    self.items(chunk.len())?;
                    chunks.push(Value::List(chunk.to_vec()));
                    self.items(chunks.len())?;
                }
                Ok(Value::List(chunks))
            }
            ("flatten", [Value::List(values)]) => {
                self.items(values.len())?;
                let mut flattened = Vec::new();
                for value in values {
                    let Value::List(inner) = value else {
                        return Err(error("ORNA-EVAL-TYPE"));
                    };
                    self.items(inner.len())?;
                    for value in inner {
                        flattened.push(value.clone());
                        self.items(flattened.len())?;
                    }
                }
                Ok(Value::List(flattened))
            }
            ("distinct", [Value::List(values)]) | ("unique", [Value::List(values)]) => {
                self.distinct(values)
            }
            ("union", [Value::List(left), Value::List(right)]) => self.union(left, right),
            ("count", [Value::List(values)]) => self.count(values),
            ("first", [Value::List(values)]) => self.first(values),
            ("min" | "max", [Value::List(values)]) => self.extreme(name, values),
            ("sum", [Value::List(values)]) => self.sum(values),
            ("one", [Value::List(values)]) => self.one(values, None, depth),
            ("one", [Value::List(values), predicate]) => self.one(values, Some(predicate), depth),
            ("every", [Value::List(values), predicate]) => self.every(values, predicate, depth),
            ("exists", [Value::List(values), predicate]) => self.exists(values, predicate, depth),
            ("take", [Value::List(values), Value::Int(count)]) => self.take(values, count),
            ("drop", [Value::List(values), Value::Int(count)]) => self.drop(values, count),
            ("map", [Value::List(values), transform]) => self.map(values, transform, depth),
            ("flat_map", [Value::List(values), transform]) => {
                self.flat_map(values, transform, depth)
            }
            ("filter", [Value::List(values), predicate]) => self.filter(values, predicate, depth),
            ("partition", [Value::List(values), predicate]) => {
                self.partition(values, predicate, depth)
            }
            ("split_when", [Value::List(values), predicate]) => {
                self.split_when(values, predicate, depth)
            }
            ("group_by", [Value::List(values), key]) => self.group_by(values, key, depth),
            ("zip", [Value::List(left), Value::List(right)]) => self.zipped(left, right, false),
            ("zip_exact", [Value::List(left), Value::List(right)]) => {
                self.zipped(left, right, true)
            }
            ("pairs", [Value::List(values)]) => {
                self.items(values.len())?;
                let mut pairs = Vec::new();
                for pair in values.windows(2) {
                    self.items(pair.len())?;
                    pairs.push(Value::Tuple(pair.to_vec()));
                    self.items(pairs.len())?;
                }
                Ok(Value::List(pairs))
            }
            ("window", [Value::List(values), Value::Int(size)]) => {
                self.windows(values, size, &BigInt::from(1))
            }
            ("window", [Value::List(values), Value::Int(size), Value::Int(step)]) => {
                self.windows(values, size, step)
            }
            ("chunk", [_, _])
            | (
                "flatten" | "distinct" | "unique" | "pairs" | "count" | "first" | "min" | "max"
                | "sum",
                [_],
            )
            | ("one", [_] | [_, _])
            | ("every" | "exists", [_, _])
            | ("count", [_, _])
            | ("union", [_, _])
            | ("take", [_, _])
            | ("drop", [_, _])
            | ("map", [_, _])
            | ("flat_map", [_, _])
            | ("filter", [_, _])
            | ("partition", [_, _])
            | ("split_when", [_, _])
            | ("group_by", [_, _])
            | ("zip" | "zip_exact", [_, _])
            | ("window", [_, _] | [_, _, _]) => Err(error("ORNA-EVAL-TYPE")),
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
        }
    }
    fn map(
        &mut self,
        values: &[Value],
        transform: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut mapped = Vec::with_capacity(values.len());
        for value in values {
            mapped.push(self.invoke_predicate(transform, value.clone(), depth + 1)?);
            self.items(mapped.len())?;
        }
        Ok(Value::List(mapped))
    }
    fn flat_map(
        &mut self,
        values: &[Value],
        transform: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut flattened = Vec::new();
        for value in values {
            let Value::List(inner) = self.invoke_predicate(transform, value.clone(), depth + 1)?
            else {
                return Err(error("ORNA-EVAL-TYPE"));
            };
            self.items(inner.len())?;
            for value in inner {
                flattened.push(value);
                self.items(flattened.len())?;
            }
        }
        Ok(Value::List(flattened))
    }
    fn distinct(&self, values: &[Value]) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut keys = Vec::new();
        let mut unique = Vec::new();
        for value in values {
            // Equality for this fallback is equality of the canonical value,
            // not incidental host representation. Values that cannot cross the
            // canonical boundary (including callables) have no lawful key.
            // Float has no default lawful hash/equality implementation here,
            // so distinctness fails closed rather than inventing semantics.
            if value.contains_float() {
                return Err(error("ORNA-EVAL-UNSUPPORTED"));
            }
            let key = value.clone().canonical()?;
            if keys.iter().any(|existing| existing == &key) {
                continue;
            }
            keys.push(key);
            unique.push(value.clone());
            self.items(unique.len())?;
        }
        Ok(Value::List(unique))
    }
    fn union(&self, left: &[Value], right: &[Value]) -> Result<Value, EvaluationError> {
        self.items(left.len())?;
        self.items(right.len())?;
        let length = left
            .len()
            .checked_add(right.len())
            .ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        self.items(length)?;
        let mut values = Vec::with_capacity(length);
        values.extend(left.iter().cloned());
        values.extend(right.iter().cloned());
        Ok(Value::List(values))
    }
    fn count(&self, values: &[Value]) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        Ok(Value::Int(BigInt::from(values.len())))
    }
    fn sum(&self, values: &[Value]) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        if values.iter().all(|value| matches!(value, Value::Float(_))) && !values.is_empty() {
            let mut total = match values.first() {
                Some(Value::Float(value)) => f64::from_bits(*value),
                _ => unreachable!("non-empty all-float list has a first Float"),
            };
            for value in &values[1..] {
                let Value::Float(value) = value else {
                    unreachable!("all-float list was checked above")
                };
                total += f64::from_bits(*value);
            }
            return Ok(Value::Float(if total.is_nan() {
                CANONICAL_NAN_BITS
            } else {
                total.to_bits()
            }));
        }
        let mut total = BigInt::ZERO;
        for value in values {
            let Value::Int(value) = value else {
                // Decimal, mixed numeric kinds, Money and affine quantities
                // remain outside this evaluator slice until their runtime
                // contracts exist.
                return Err(error("ORNA-EVAL-UNSUPPORTED"));
            };
            total = self.integer(&total + value)?;
        }
        Ok(Value::Int(total))
    }
    fn extreme(&self, name: &str, values: &[Value]) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        if values.iter().all(|value| matches!(value, Value::Float(_))) {
            let bits = values
                .iter()
                .map(|value| match value {
                    Value::Float(bits) => *bits,
                    _ => unreachable!("all-float list was checked above"),
                })
                .collect::<Vec<_>>();
            let result = if name == "min" {
                float_min(&bits)
            } else {
                float_max(&bits)
            };
            return Ok(result.map_or(Value::Null, |value| {
                Value::Option(Some(Box::new(Value::Float(value))))
            }));
        }
        let mut candidate = None;
        for value in values {
            let Value::Int(value) = value else {
                // Mixed numeric kinds, Decimal, Money and affine aggregation
                // fail closed rather than receiving incidental host ordering.
                return Err(error("ORNA-EVAL-UNSUPPORTED"));
            };
            let replace = candidate.as_ref().is_none_or(|current: &BigInt| {
                if name == "min" {
                    value < current
                } else {
                    value > current
                }
            });
            if replace {
                // Strict comparison above deliberately retains the first
                // equal candidate, preserving observable input order.
                candidate = Some(value.clone());
            }
        }
        Ok(candidate.map_or(Value::Null, |value| {
            Value::Option(Some(Box::new(Value::Int(value))))
        }))
    }
    fn first(&self, values: &[Value]) -> Result<Value, EvaluationError> {
        Ok(values.first().cloned().unwrap_or(Value::Null))
    }
    fn one(
        &mut self,
        values: &[Value],
        predicate: Option<&Value>,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let Some(predicate) = predicate else {
            return match values {
                [] => Err(error("ORNA-EVAL-RELATION-ONE-ZERO")),
                [value] => Ok(value.clone()),
                _ => Err(error("ORNA-EVAL-RELATION-ONE-MULTIPLE")),
            };
        };

        let mut matching = None;
        for value in values {
            match self.invoke_predicate(predicate, value.clone(), depth + 1)? {
                Value::Bool(true) => {
                    if matching.is_some() {
                        // The second matching value is sufficient to classify
                        // the cardinality failure; do not invoke the callback
                        // for any later input value.
                        return Err(error("ORNA-EVAL-RELATION-ONE-MULTIPLE"));
                    }
                    matching = Some(value.clone());
                }
                Value::Bool(false) => {}
                _ => return Err(error("ORNA-EVAL-TYPE")),
            }
        }
        matching.ok_or_else(|| error("ORNA-EVAL-RELATION-ONE-ZERO"))
    }
    fn every(
        &mut self,
        values: &[Value],
        predicate: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        for value in values {
            match self.invoke_predicate(predicate, value.clone(), depth + 1)? {
                Value::Bool(true) => {}
                Value::Bool(false) => return Ok(Value::Bool(false)),
                _ => return Err(error("ORNA-EVAL-TYPE")),
            }
        }
        Ok(Value::Bool(true))
    }
    fn exists(
        &mut self,
        values: &[Value],
        predicate: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        for value in values {
            match self.invoke_predicate(predicate, value.clone(), depth + 1)? {
                Value::Bool(true) => return Ok(Value::Bool(true)),
                Value::Bool(false) => {}
                _ => return Err(error("ORNA-EVAL-TYPE")),
            }
        }
        Ok(Value::Bool(false))
    }
    fn take(&self, values: &[Value], count: &BigInt) -> Result<Value, EvaluationError> {
        if count.is_negative() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        let end = if count >= &BigInt::from(values.len()) {
            values.len()
        } else {
            count.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?
        };
        self.items(end)?;
        Ok(Value::List(values[..end].to_vec()))
    }
    fn drop(&self, values: &[Value], count: &BigInt) -> Result<Value, EvaluationError> {
        if count.is_negative() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        let start = if count >= &BigInt::from(values.len()) {
            values.len()
        } else {
            count.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?
        };
        self.items(values.len() - start)?;
        Ok(Value::List(values[start..].to_vec()))
    }
    fn filter(
        &mut self,
        values: &[Value],
        predicate: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut filtered = Vec::new();
        for value in values {
            // Invoke once, in input order, and retain only an explicit true.
            match self.invoke_predicate(predicate, value.clone(), depth + 1)? {
                Value::Bool(true) => {
                    filtered.push(value.clone());
                    self.items(filtered.len())?;
                }
                Value::Bool(false) => {}
                _ => return Err(error("ORNA-EVAL-TYPE")),
            }
        }
        Ok(Value::List(filtered))
    }
    fn partition(
        &mut self,
        values: &[Value],
        predicate: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        self.items(2)?;
        let mut matching = Vec::new();
        let mut remaining = Vec::new();
        for value in values {
            // Invoke once, in input order, so a lawful callback's observable
            // behavior is never duplicated by classification.
            match self.invoke_predicate(predicate, value.clone(), depth + 1)? {
                Value::Bool(true) => {
                    matching.push(value.clone());
                    self.items(matching.len())?;
                }
                Value::Bool(false) => {
                    remaining.push(value.clone());
                    self.items(remaining.len())?;
                }
                _ => return Err(error("ORNA-EVAL-TYPE")),
            }
        }
        Ok(Value::Tuple(vec![
            Value::List(matching),
            Value::List(remaining),
        ]))
    }
    fn split_when(
        &mut self,
        values: &[Value],
        predicate: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut groups = Vec::new();
        let mut current = Vec::new();
        for value in values {
            // Invoke once, in input order. A true boundary starts the next
            // group, except at the beginning where no empty group is lawful.
            match self.invoke_predicate(predicate, value.clone(), depth + 1)? {
                Value::Bool(true) if !current.is_empty() => {
                    groups.push(Value::List(std::mem::take(&mut current)));
                    self.items(groups.len())?;
                }
                Value::Bool(_) => {}
                _ => return Err(error("ORNA-EVAL-TYPE")),
            }
            current.push(value.clone());
            self.items(current.len())?;
        }
        if !current.is_empty() {
            groups.push(Value::List(current));
            self.items(groups.len())?;
        }
        Ok(Value::List(groups))
    }
    fn group_by(
        &mut self,
        values: &[Value],
        key: &Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let mut groups = Vec::<(Value, Vec<Value>)>::new();
        for value in values {
            // Classify once, in input order. The returned key must have an
            // explicit total comparison; canonical encoding alone is not a
            // substitute for the library's lawful-key requirement.
            let group_key = self.invoke_predicate(key, value.clone(), depth + 1)?;
            lawful_group_key(&group_key)?;
            let mut matched = false;
            for (existing_key, rows) in &mut groups {
                match compare_group_keys(existing_key, &group_key)? {
                    std::cmp::Ordering::Equal => {
                        rows.push(value.clone());
                        self.items(rows.len())?;
                        matched = true;
                        break;
                    }
                    _ => {}
                }
            }
            if !matched {
                self.items(groups.len() + 1)?;
                groups.push((group_key, vec![value.clone()]));
            }
        }

        // Insertion sort keeps the ordering decision fallible and avoids
        // accepting an incidental host ordering for source values.
        for index in 1..groups.len() {
            let mut current = index;
            while current > 0
                && compare_group_keys(&groups[current - 1].0, &groups[current].0)?
                    == std::cmp::Ordering::Greater
            {
                groups.swap(current - 1, current);
                current -= 1;
            }
        }
        self.items(groups.len())?;
        Ok(Value::List(
            groups
                .into_iter()
                .map(|(key, rows)| Value::Tuple(vec![key, Value::List(rows)]))
                .collect(),
        ))
    }
    fn invoke_predicate(
        &mut self,
        callable: &Value,
        input: Value,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        let (parameters, body, captured, session_owned, namespace) = match callable {
            Value::Function(name) => {
                let function = self
                    .functions
                    .get(name)
                    .ok_or_else(|| error("ORNA-EVAL-NAME"))?;
                (
                    function.parameters.clone(),
                    function.body.clone(),
                    Scope::from_environment(&function.environment, self)?,
                    self.session_functions
                        .is_some_and(|functions| functions.contains(name)),
                    function_namespace(name),
                )
            }
            Value::Closure(closure) => (
                closure.parameters.clone(),
                closure.body.clone(),
                closure.captured.clone(),
                self.repl_bindings,
                None,
            ),
            _ => return Err(error("ORNA-EVAL-TYPE")),
        };
        self.depth(depth + 1)?;
        self.items(1)?;
        let Some(parameter) = parameters.first() else {
            return Err(error("ORNA-EVAL-ARGUMENT"));
        };
        let mut supplied = BTreeMap::new();
        supplied.insert(parameter_key(parameter, 0), input);
        let previous_repl_bindings = self.repl_bindings;
        self.repl_bindings = session_owned;
        let previous_namespace = self.namespace.clone();
        self.namespace = namespace;
        let result = invoke_pure(self, &parameters, &body, captured, supplied, depth + 1);
        self.namespace = previous_namespace;
        self.repl_bindings = previous_repl_bindings;
        result
    }
    fn positive_collection_size(&self, value: &BigInt) -> Result<usize, EvaluationError> {
        if !value.is_positive() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        Ok(value.to_usize().unwrap_or(usize::MAX))
    }
    fn zipped(
        &self,
        left: &[Value],
        right: &[Value],
        exact: bool,
    ) -> Result<Value, EvaluationError> {
        self.items(left.len())?;
        self.items(right.len())?;
        if exact && left.len() != right.len() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        let mut pairs = Vec::new();
        for (left, right) in left.iter().zip(right) {
            self.items(2)?;
            pairs.push(Value::Tuple(vec![left.clone(), right.clone()]));
            self.items(pairs.len())?;
        }
        Ok(Value::List(pairs))
    }
    fn windows(
        &self,
        values: &[Value],
        size: &BigInt,
        step: &BigInt,
    ) -> Result<Value, EvaluationError> {
        self.items(values.len())?;
        let size = self.positive_collection_size(size)?;
        let step = self.positive_collection_size(step)?;
        if size > values.len() {
            return Ok(Value::List(Vec::new()));
        }
        let last_start = values.len() - size;
        let mut start = 0;
        let mut windows = Vec::new();
        while start <= last_start {
            let window = &values[start..start + size];
            self.items(window.len())?;
            windows.push(Value::List(window.to_vec()));
            self.items(windows.len())?;
            let Some(next) = start.checked_add(step) else {
                break;
            };
            start = next;
        }
        Ok(Value::List(windows))
    }
    fn shift_left(&self, value: &BigInt, count: &BigInt) -> Result<Value, EvaluationError> {
        if count.is_negative() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        if value.is_zero() {
            return Ok(Value::Int(BigInt::ZERO));
        }
        let max_bits = self.limits.max_integer_digits.saturating_mul(4);
        if BigInt::from(value.bits()) + count > BigInt::from(max_bits) {
            return Err(error("ORNA-EVAL-LIMIT"));
        }
        let count = count.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        Ok(Value::Int(self.integer(value << count)?))
    }
    fn shift_right(&self, value: &BigInt, count: &BigInt) -> Result<Value, EvaluationError> {
        if count.is_negative() {
            return Err(error("ORNA-EVAL-VALUE"));
        }
        if count >= &BigInt::from(value.bits()) {
            return Ok(Value::Int(if value.is_negative() {
                BigInt::from(-1)
            } else {
                BigInt::ZERO
            }));
        }
        let count = count.to_usize().ok_or_else(|| error("ORNA-EVAL-LIMIT"))?;
        Ok(Value::Int(self.integer(value >> count)?))
    }
}

fn pattern_names(pattern: &Pattern) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    collect_pattern_names(pattern, &mut names);
    names
}

fn collect_pattern_names(pattern: &Pattern, names: &mut BTreeSet<String>) {
    match pattern {
        Pattern::Name(name, _) => {
            names.insert(name.clone());
        }
        Pattern::Record { fields, .. } => {
            for (name, pattern, _) in fields {
                if let Some(pattern) = pattern {
                    collect_pattern_names(pattern, names);
                } else {
                    names.insert(name.clone());
                }
            }
        }
        Pattern::Tuple { elements, .. } | Pattern::List { elements, .. } => {
            for pattern in elements {
                collect_pattern_names(pattern, names);
            }
        }
        Pattern::Constructor {
            arguments, fields, ..
        } => {
            for pattern in arguments {
                collect_pattern_names(pattern, names);
            }
            for field in fields {
                if let Some(pattern) = &field.pattern {
                    collect_pattern_names(pattern, names);
                } else {
                    names.insert(field.name.clone());
                }
            }
        }
        Pattern::Wildcard(_) | Pattern::Literal { .. } => {}
    }
}

fn bind(
    pattern: &Pattern,
    value: Value,
    scope: &mut Scope,
    context: &Context,
    depth: usize,
) -> Result<bool, EvaluationError> {
    context.depth(depth)?;
    match pattern {
        Pattern::Name(name, _) => {
            if name == "_" {
                return Ok(true);
            }
            scope.0.insert(name.clone(), value);
            scope.1.remove(name);
            scope.2.remove(name);
            Ok(true)
        }
        Pattern::Wildcard(_) => Ok(true),
        Pattern::Literal {
            kind: LiteralKind::Null,
            ..
        } if matches!(value, Value::Option(None)) => Ok(true),
        Pattern::Literal { text, kind, .. } => Ok(context.literal(text, *kind)? == value),
        Pattern::Tuple { elements, .. } => match value {
            Value::Tuple(values) if values.len() == elements.len() => {
                context.items(values.len())?;
                for (pattern, value) in elements.iter().zip(values) {
                    if !bind(pattern, value, scope, context, depth + 1)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            _ => Ok(false),
        },
        Pattern::List { elements, .. } => match value {
            Value::List(values) if values.len() == elements.len() => {
                context.items(values.len())?;
                for (pattern, value) in elements.iter().zip(values) {
                    if !bind(pattern, value, scope, context, depth + 1)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            _ => Ok(false),
        },
        Pattern::Record { fields, .. } => match value {
            Value::Record(values) => {
                context.items(fields.len())?;
                for (name, pattern, _) in fields {
                    let Some(value) = values.get(name).cloned() else {
                        return Ok(false);
                    };
                    if let Some(pattern) = pattern {
                        if !bind(pattern, value, scope, context, depth + 1)? {
                            return Ok(false);
                        }
                    } else {
                        scope.0.insert(name.clone(), value);
                        scope.1.remove(name);
                    }
                }
                Ok(true)
            }
            _ => Ok(false),
        },
        Pattern::Constructor {
            path,
            arguments,
            fields,
            ..
        } => bind_constructor(path, arguments, fields, value, scope, context, depth),
    }
}
fn bind_constructor(
    path: &[orna_syntax_v1::NameSegment],
    arguments: &[Pattern],
    fields: &[PatternField],
    value: Value,
    scope: &mut Scope,
    context: &Context,
    depth: usize,
) -> Result<bool, EvaluationError> {
    context.items(path.len())?;
    context.items(arguments.len())?;
    context.items(fields.len())?;
    if path.len() == 1 && path[0].text == "Some" {
        let [argument] = arguments else {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        };
        if !fields.is_empty() {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        return match value {
            Value::Option(Some(value)) => bind(argument, *value, scope, context, depth + 1),
            Value::Option(None) => Ok(false),
            _ => Ok(false),
        };
    }
    if path.len() < 2 || !arguments.is_empty() {
        return Err(error("ORNA-EVAL-UNSUPPORTED"));
    }
    let qualified_name = path
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join(".");
    let qualified_name = context.string(qualified_name)?;
    let Some(Value::Enum {
        type_id: expected_type,
        variant_id: expected_variant,
        ..
    }) = scope.0.get(&qualified_name)
    else {
        return Err(error("ORNA-EVAL-UNSUPPORTED"));
    };
    let Value::Enum {
        type_id,
        variant_id,
        payload,
    } = value
    else {
        return Ok(false);
    };
    if type_id != *expected_type || variant_id != *expected_variant {
        return Ok(false);
    }
    match (fields, payload) {
        ([], None) => Ok(true),
        ([], Some(_)) | (_, None) => Ok(false),
        (_, Some(payload)) => bind_pattern_fields(fields, *payload, scope, context, depth + 1),
    }
}
fn bind_pattern_fields(
    patterns: &[PatternField],
    value: Value,
    scope: &mut Scope,
    context: &Context,
    depth: usize,
) -> Result<bool, EvaluationError> {
    context.depth(depth)?;
    let Value::NominalRecord { fields, .. } = value else {
        return Ok(false);
    };
    for field in patterns {
        let Some((_, value)) = fields
            .iter()
            .find(|(key, _)| matches!(key, Raw::Text(name) if name == &field.name))
        else {
            return Ok(false);
        };
        if let Some(pattern) = &field.pattern {
            if !bind(pattern, value.clone(), scope, context, depth + 1)? {
                return Ok(false);
            }
        } else {
            scope.0.insert(field.name.clone(), value.clone());
            scope.1.remove(&field.name);
        }
    }
    Ok(true)
}
fn named_arguments(
    function: &str,
    arguments: &[orna_syntax_v1::Argument],
    values: Vec<Value>,
    implicit: usize,
    collection: bool,
) -> Result<Vec<Value>, EvaluationError> {
    if arguments.iter().all(|argument| argument.name.is_none()) {
        return Ok(values);
    }
    let expected: &[&str] = match function {
        "increment" | "decrement" | "is_zero" => &["value"],
        "min" | "max" if collection => &["rows"],
        "min" | "max" => &["left", "right"],
        "clamp" => &["value", "min", "max"],
        "trim" | "lower" | "upper" => &["value"],
        "split" => &["value", "separator"],
        "join" => &["values", "separator"],
        "starts_with" => &["value", "prefix"],
        "ends_with" => &["value", "suffix"],
        "contains" => &["value", "needle"],
        "replace" => &["value", "from", "to"],
        "normalise" => &["value", "form"],
        "chunk" => &["values", "size"],
        "flatten" | "distinct" | "unique" | "pairs" | "count" => &["values"],
        "sum" => &["rows"],
        "first" => &["rows"],
        "one" => match values.len() {
            1 => &["rows"],
            2 => &["rows", "predicate"],
            _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
        },
        "every" | "exists" => &["rows", "predicate"],
        "union" => &["left", "right"],
        "take" => &["values", "count"],
        "drop" => &["values", "count"],
        "map" | "flat_map" => &["values", "transform"],
        "filter" | "partition" | "split_when" => &["values", "predicate"],
        "group_by" => &["values", "key"],
        "zip" | "zip_exact" => &["left", "right"],
        "window" => match values.len() {
            2 => &["values", "size"],
            3 => &["values", "size", "step"],
            _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
        },
        _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
    };
    if values.len() != expected.len() {
        return Err(error("ORNA-EVAL-UNSUPPORTED"));
    }
    let mut ordered = vec![None; expected.len()];
    let mut positional = 0;
    let mut named_started = false;
    for (index, value) in values.into_iter().enumerate() {
        let name = index
            .checked_sub(implicit)
            .and_then(|index| arguments.get(index))
            .and_then(|argument| argument.name.as_deref());
        let position = if let Some(name) = name {
            named_started = true;
            expected.iter().position(|expected| *expected == name)
        } else if named_started {
            None
        } else {
            let position = Some(positional);
            positional += 1;
            position
        }
        .ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?;
        if ordered[position].replace(value).is_some() {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
    }
    ordered
        .into_iter()
        .map(|value| value.ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED")))
        .collect()
}
fn math_name(expression: &Expr) -> Option<&str> {
    standard_name(expression, "math")
}

fn bits_name(expression: &Expr) -> Option<&str> {
    standard_name(expression, "bits")
}

fn text_name(expression: &Expr) -> Option<&str> {
    standard_name(expression, "text")
}

fn collection_name(expression: &Expr) -> Option<&str> {
    standard_name(expression, "collection")
}

fn root_collection_name(expression: &Expr) -> Option<&str> {
    let Expr::Name { text, .. } = expression else {
        return None;
    };
    matches!(
        text.as_str(),
        "first" | "one" | "min" | "max" | "sum" | "every" | "exists" | "map" | "flat_map"
    )
    .then_some(text.as_str())
}

fn standard_name<'a>(expression: &'a Expr, module: &str) -> Option<&'a str> {
    let Expr::Field { base, name, .. } = expression else {
        return None;
    };
    let Expr::Field {
        base,
        name: selected_module,
        ..
    } = base.as_ref()
    else {
        return None;
    };
    let Expr::Name { text, .. } = base.as_ref() else {
        return None;
    };
    (text == "std" && selected_module == module).then_some(name.as_str())
}

fn function_name(expression: &Expr) -> Option<String> {
    match expression {
        Expr::Name { text, .. } => Some(text.clone()),
        Expr::Field { base, name, .. } => Some(format!("{}.{}", function_name(base)?, name)),
        _ => None,
    }
}

fn function_namespace(name: &str) -> Option<String> {
    name.rsplit_once('.')
        .map(|(namespace, _)| namespace.to_owned())
}

fn function_root_name(expression: &Expr) -> Option<&str> {
    match expression {
        Expr::Name { text, .. } => Some(text),
        Expr::Field { base, .. } => function_root_name(base),
        _ => None,
    }
}

fn one_like(value: &Value) -> Result<Value, EvaluationError> {
    match value {
        Value::Int(_) => Ok(Value::Int(1.into())),
        Value::Decimal(_) => DecimalValue::new(1.into(), 0.into()).map(Value::Decimal),
        Value::Float(_) => Ok(Value::Float(1.0f64.to_bits())),
        _ => Err(error("ORNA-EVAL-TYPE")),
    }
}
fn raw_option_int(value: Option<BigInt>) -> Raw {
    Raw::Tag(
        60013,
        Box::new(Raw::Array(match value {
            Some(value) => vec![Raw::Int(1.into()), Raw::Int(value)],
            None => vec![Raw::Int(0.into())],
        })),
    )
}
fn option_int(
    raw: &Raw,
    context: &mut Context,
    depth: usize,
) -> Result<Option<BigInt>, EvaluationError> {
    context.depth(depth)?;
    let Raw::Tag(60013, boxed) = raw else {
        return Err(error("ORNA-EVAL-VALUE"));
    };
    let Raw::Array(parts) = boxed.as_ref() else {
        return Err(error("ORNA-EVAL-VALUE"));
    };
    match parts.as_slice() {
        [Raw::Int(tag)] if tag.is_zero() => Ok(None),
        [Raw::Int(tag), Raw::Int(value)] if *tag == BigInt::from(1) => {
            context.integer(value.clone()).map(Some)
        }
        [Raw::Int(tag), _] if *tag == BigInt::from(1) => Err(error("ORNA-EVAL-TYPE")),
        _ => Err(error("ORNA-EVAL-VALUE")),
    }
}
fn compare(op: &str, ordering: std::cmp::Ordering) -> Result<Value, EvaluationError> {
    Ok(Value::Bool(match op {
        "<" => ordering.is_lt(),
        "<=" => ordering.is_le(),
        ">" => ordering.is_gt(),
        ">=" => ordering.is_ge(),
        _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
    }))
}
fn compare_values(left: &Value, right: &Value) -> Result<std::cmp::Ordering, EvaluationError> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => Ok(a.cmp(b)),
        (Value::Decimal(a), Value::Decimal(b)) => {
            let delta = &a.exponent10 - &b.exponent10;
            if delta
                .abs()
                .to_usize()
                .is_none_or(|value| value > DEFAULT_INTEGER_DIGITS)
            {
                return Err(error("ORNA-EVAL-LIMIT"));
            }
            let factor = BigInt::from(10u8).pow(delta.abs().to_u32().unwrap_or(0));
            Ok(if delta.sign() == Sign::Minus {
                a.coefficient.cmp(&(&b.coefficient * factor))
            } else {
                (&a.coefficient * factor).cmp(&b.coefficient)
            })
        }
        (Value::Float(a), Value::Float(b)) => f64::from_bits(*a)
            .partial_cmp(&f64::from_bits(*b))
            .ok_or_else(|| error("ORNA-EVAL-VALUE")),
        (
            Value::Range {
                lower: left_lower,
                upper: left_upper,
                upper_inclusive: left_inclusive,
            },
            Value::Range {
                lower: right_lower,
                upper: right_upper,
                upper_inclusive: right_inclusive,
            },
        ) => Ok(left_lower
            .cmp(right_lower)
            .then_with(|| match (left_upper, right_upper) {
                (None, None) => std::cmp::Ordering::Equal,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (Some(_), None) => std::cmp::Ordering::Less,
                (Some(left), Some(right)) => left.cmp(right),
            })
            .then_with(|| left_inclusive.cmp(right_inclusive))),
        _ => Err(error("ORNA-EVAL-TYPE")),
    }
}
fn lawful_group_key(value: &Value) -> Result<(), EvaluationError> {
    match value {
        Value::Bool(_) | Value::Int(_) | Value::Decimal(_) | Value::String(_) => Ok(()),
        Value::Tuple(values) => values.iter().try_for_each(lawful_group_key),
        _ => Err(error("ORNA-EVAL-TYPE")),
    }
}
fn compare_group_keys(left: &Value, right: &Value) -> Result<std::cmp::Ordering, EvaluationError> {
    match (left, right) {
        (Value::Bool(left), Value::Bool(right)) => Ok(left.cmp(right)),
        (Value::Int(left), Value::Int(right)) => Ok(left.cmp(right)),
        (Value::Decimal(left), Value::Decimal(right)) => compare_values(
            &Value::Decimal(left.clone()),
            &Value::Decimal(right.clone()),
        ),
        (Value::String(left), Value::String(right)) => Ok(left.cmp(right)),
        (Value::Tuple(left), Value::Tuple(right)) => {
            for (left, right) in left.iter().zip(right) {
                let ordering = compare_group_keys(left, right)?;
                if ordering != std::cmp::Ordering::Equal {
                    return Ok(ordering);
                }
            }
            Ok(left.len().cmp(&right.len()))
        }
        _ => Err(error("ORNA-EVAL-TYPE")),
    }
}
fn ordered(left: &Value, right: &Value, min: bool) -> Result<Value, EvaluationError> {
    let ordering = compare_values(left, right)?;
    if (ordering.is_le()) == min {
        Ok(left.clone())
    } else {
        Ok(right.clone())
    }
}
fn finite_float(value: f64) -> Result<Value, EvaluationError> {
    if value.is_finite() {
        Ok(Value::Float(value.to_bits()))
    } else {
        Err(error("ORNA-EVAL-VALUE"))
    }
}
fn parse_int(text: &str) -> Result<BigInt, EvaluationError> {
    let text = text.replace('_', "");
    if let Some(value) = text.strip_prefix("0x") {
        BigInt::parse_bytes(value.as_bytes(), 16).ok_or_else(|| error("ORNA-EVAL-VALUE"))
    } else if let Some(value) = text.strip_prefix("0b") {
        BigInt::parse_bytes(value.as_bytes(), 2).ok_or_else(|| error("ORNA-EVAL-VALUE"))
    } else {
        BigInt::parse_bytes(text.as_bytes(), 10).ok_or_else(|| error("ORNA-EVAL-VALUE"))
    }
}
fn parse_decimal(text: &str) -> Result<(BigInt, BigInt), EvaluationError> {
    let text = text.replace('_', "");
    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(index) => (
            &text[..index],
            text[index + 1..]
                .parse::<i64>()
                .map_err(|_| error("ORNA-EVAL-VALUE"))?,
        ),
        None => (text.as_str(), 0),
    };
    let (whole, fraction) = mantissa
        .split_once('.')
        .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
    let coefficient = BigInt::parse_bytes(format!("{whole}{fraction}").as_bytes(), 10)
        .ok_or_else(|| error("ORNA-EVAL-VALUE"))?;
    Ok((
        coefficient,
        BigInt::from(
            exponent - i64::try_from(fraction.len()).map_err(|_| error("ORNA-EVAL-LIMIT"))?,
        ),
    ))
}
fn unescape_string(text: &str) -> Result<String, EvaluationError> {
    if text.len() < 2 {
        return Err(error("ORNA-EVAL-VALUE"));
    }
    unescape_string_body(&text[1..text.len() - 1])
}
fn unescape_string_body(body: &str) -> Result<String, EvaluationError> {
    let mut output = String::new();
    let mut characters = body.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        match characters.next().ok_or_else(|| error("ORNA-EVAL-VALUE"))? {
            'n' => output.push('\n'),
            'r' => output.push('\r'),
            't' => output.push('\t'),
            '\\' => output.push('\\'),
            '"' => output.push('"'),
            _ => return Err(error("ORNA-EVAL-VALUE")),
        }
    }
    Ok(output)
}
