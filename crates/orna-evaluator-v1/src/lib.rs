//! A deliberately small, deterministic Orna 1.0 expression evaluator.
//!
//! The public boundary admits and returns only OVB-1 canonical values. It has
//! no I/O, external mutation, clock, random, module-loading, or host-call
//! capability.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use num_bigint::{BigInt, Sign};
use num_integer::Integer;
use num_traits::{Signed, ToPrimitive, Zero};
use orna_foundation_v1::{CanonicalValue, Diagnostic, DiagnosticSeverity, SafeText};
use orna_syntax_v1::{
    AssignmentOperator, AssignmentTarget, ControlKind, Expr, LiteralKind, Parameter, Pattern,
    PatternField, ReplInput, Statement, StringSegment, parse_expression, parse_repl,
};
use orna_value_v1::Raw;

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

/// Evaluate one expression using [`parse_expression`].
pub fn evaluate_expression(
    source: &str,
    environment: &Environment,
    limits: Limits,
) -> Result<CanonicalValue, EvaluationError> {
    check_limits(source, limits)?;
    let parsed = parse_expression(source);
    if !parsed.is_ok() {
        return Err(error("ORNA-EVAL-PARSE"));
    }
    evaluate_parsed(&parsed.value, environment, limits)
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
    };
    context.items(functions.len())?;
    let mut scope = Scope::from_environment(environment, &mut context)?;
    context
        .evaluate(expression, &mut scope, 0)
        .and_then(Value::canonical)
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
    };
    let supplied = Scope::from_environment(arguments, &mut context)?;
    invoke_pure(&mut context, parameters, body, environment, supplied, 0).and_then(Value::canonical)
}

fn invoke_pure(
    context: &mut Context,
    parameters: &[Parameter],
    body: &Expr,
    environment: &Environment,
    supplied: Scope,
    depth: usize,
) -> Result<Value, EvaluationError> {
    context.depth(depth)?;
    context.items(parameters.len())?;
    context.items(supplied.0.len())?;
    let mut names = BTreeSet::new();
    for parameter in parameters {
        let Pattern::Name(name, _) = &parameter.pattern else {
            return Err(error("ORNA-EVAL-ARGUMENT"));
        };
        if name.len() > context.limits.max_string_bytes {
            return Err(error("ORNA-EVAL-LIMIT"));
        }
        if !names.insert(name.as_str())
            || (!supplied.0.contains_key(name) && parameter.default.is_none())
        {
            return Err(error("ORNA-EVAL-ARGUMENT"));
        }
    }
    if supplied.0.keys().any(|name| !names.contains(name.as_str())) {
        return Err(error("ORNA-EVAL-ARGUMENT"));
    }
    let mut scope = Scope::from_environment(environment, context)?;
    for parameter in parameters {
        let Pattern::Name(name, _) = &parameter.pattern else {
            unreachable!("parameter patterns were admitted above");
        };
        let value = if let Some(value) = supplied.0.get(name) {
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
        scope.0.insert(name.clone(), value);
        context.items(scope.0.len())?;
    }
    context.evaluate(body, &mut scope, depth)
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
    EvaluationError {
        diagnostic: Box::new(
            Diagnostic::new(
                SafeText::new(code).expect("static safe code"),
                DiagnosticSeverity::Error,
                SafeText::redacted(),
            )
            .expect("static diagnostic")
            .redacted(),
        ),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Value {
    Null,
    Bool(bool),
    Int(BigInt),
    Decimal(DecimalValue),
    Float(u64),
    String(String),
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
    fn canonical(self) -> Result<CanonicalValue, EvaluationError> {
        CanonicalValue::new(self.raw()).map_err(|_| error("ORNA-EVAL-VALUE"))
    }
    fn raw(self) -> Raw {
        match self {
            Self::Null => Raw::Null,
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
            Self::List(values) | Self::Tuple(values) => {
                Raw::Array(values.into_iter().map(Value::raw).collect())
            }
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
                        .map(|(key, value)| (Raw::Text(key), value.raw()))
                        .collect(),
                )
            }
            Self::NominalRecord { type_id, fields } => Raw::Tag(
                60009,
                Box::new(Raw::Array(vec![
                    type_id,
                    Raw::Array(
                        fields
                            .into_iter()
                            .map(|(key, value)| Raw::Array(vec![key, value.raw()]))
                            .collect(),
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
                    payload.map_or(Raw::Null, |value| value.raw()),
                ])),
            ),
            Self::Option(value) => Raw::Tag(
                60013,
                Box::new(match value {
                    Some(value) => Raw::Array(vec![Raw::Int(1.into()), value.raw()]),
                    None => Raw::Array(vec![Raw::Int(0.into())]),
                }),
            ),
        }
    }
    fn from_canonical(
        value: &CanonicalValue,
        context: &mut Context,
        depth: usize,
    ) -> Result<Self, EvaluationError> {
        context.depth(depth)?;
        match value.raw() {
            Raw::Null => Ok(Self::Null),
            Raw::Bool(value) => Ok(Self::Bool(*value)),
            Raw::Int(value) => context.integer(value.clone()).map(Self::Int),
            Raw::Float(bits) if f64::from_bits(*bits).is_finite() => Ok(Self::Float(*bits)),
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
}

#[derive(Clone)]
struct Scope(BTreeMap<String, Value>);
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
        Ok(Self(values))
    }
}

struct Context<'a> {
    limits: Limits,
    steps: u64,
    functions: &'a Functions,
}
impl Context<'_> {
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
        self.step()?;
        self.depth(depth)?;
        match expression {
            Expr::Name { text, .. } => scope
                .0
                .get(text)
                .cloned()
                .ok_or_else(|| error("ORNA-EVAL-NAME")),
            Expr::Literal { text, kind, .. } => self.literal(text, *kind),
            Expr::InterpolatedString { segments, .. } => {
                self.interpolated_string(segments, scope, depth)
            }
            Expr::Group { inner, .. } => self.evaluate(inner, scope, depth + 1),
            Expr::Unary { op, rhs, .. } => {
                let value = self.evaluate(rhs, scope, depth + 1)?;
                self.unary(op, value)
            }
            Expr::Binary { lhs, op, rhs, .. } => self.binary(op, lhs, rhs, scope, depth),
            Expr::List { elements, .. } => self.sequence(elements, scope, depth).map(Value::List),
            Expr::Tuple { elements, .. } => self.sequence(elements, scope, depth).map(Value::Tuple),
            Expr::Record { fields, .. } => {
                self.items(fields.len())?;
                let mut result = BTreeMap::new();
                for field in fields {
                    if result
                        .insert(
                            field.name.clone(),
                            self.evaluate(&field.value, scope, depth + 1)?,
                        )
                        .is_some()
                    {
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
            } => self.call(callee, arguments, scope, depth),
            Expr::Index { base, index, .. } => {
                let base = self.evaluate(base, scope, depth + 1)?;
                let index = self.evaluate(index, scope, depth + 1)?;
                self.index(base, index)
            }
            Expr::Field { base, name, .. } => {
                let Value::Record(fields) = self.evaluate(base, scope, depth + 1)? else {
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
                let Value::List(values) = self.evaluate(iterable, scope, depth + 1)? else {
                    return Err(error("ORNA-EVAL-TYPE"));
                };
                self.items(values.len())?;
                let outer_names = scope.0.keys().cloned().collect::<Vec<_>>();
                let bound_names = pattern_names(binding);
                for value in values {
                    let mut iteration = scope.clone();
                    if !bind(binding, value, &mut iteration, self, depth + 1)? {
                        return Err(error("ORNA-EVAL-TYPE"));
                    }
                    self.evaluate(body, &mut iteration, depth + 1)?;
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
            _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
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
        elements
            .iter()
            .map(|element| self.evaluate(element, scope, depth + 1))
            .collect()
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
                _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
            }
        }
        let result = tail.map_or(Ok(Value::Null), |value| {
            self.evaluate(value, &mut local, depth + 1)
        })?;
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
        let current = scope
            .0
            .get(name)
            .cloned()
            .ok_or_else(|| error("ORNA-EVAL-NAME"))?;
        let value = self.evaluate(expression, scope, depth + 1)?;
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
            "??" => match self.evaluate(lhs, scope, depth + 1)? {
                Value::Option(Some(value)) => Ok(*value),
                Value::Option(None) | Value::Null => self.evaluate(rhs, scope, depth + 1),
                _ => Err(error("ORNA-EVAL-TYPE")),
            },
            "&&" => match self.evaluate(lhs, scope, depth + 1)? {
                Value::Bool(false) => Ok(Value::Bool(false)),
                Value::Bool(true) => match self.evaluate(rhs, scope, depth + 1)? {
                    Value::Bool(value) => Ok(Value::Bool(value)),
                    _ => Err(error("ORNA-EVAL-TYPE")),
                },
                _ => Err(error("ORNA-EVAL-TYPE")),
            },
            "||" => match self.evaluate(lhs, scope, depth + 1)? {
                Value::Bool(true) => Ok(Value::Bool(true)),
                Value::Bool(false) => match self.evaluate(rhs, scope, depth + 1)? {
                    Value::Bool(value) => Ok(Value::Bool(value)),
                    _ => Err(error("ORNA-EVAL-TYPE")),
                },
                _ => Err(error("ORNA-EVAL-TYPE")),
            },
            _ => {
                let left = self.evaluate(lhs, scope, depth + 1)?;
                let right = self.evaluate(rhs, scope, depth + 1)?;
                self.apply_binary(op, left, right)
            }
        }
    }
    fn apply_binary(&self, op: &str, left: Value, right: Value) -> Result<Value, EvaluationError> {
        if matches!(op, "==" | "!=") {
            let equal = left == right;
            return Ok(Value::Bool(if op == "==" { equal } else { !equal }));
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
        scope: &mut Scope,
        depth: usize,
    ) -> Result<Value, EvaluationError> {
        if let Expr::Name { text, .. } = callee {
            if scope.0.contains_key(text) {
                return Err(error("ORNA-EVAL-TYPE"));
            }
            let functions = self.functions;
            let function = functions.get(text).ok_or_else(|| error("ORNA-EVAL-NAME"))?;
            self.depth(depth + 1)?;
            self.items(arguments.len())?;
            let mut supplied = BTreeMap::new();
            let mut positional = 0;
            let mut named_started = false;
            for argument in arguments {
                let name = if let Some(name) = &argument.name {
                    named_started = true;
                    name
                } else {
                    if named_started {
                        return Err(error("ORNA-EVAL-ARGUMENT"));
                    }
                    let Some(Parameter {
                        pattern: Pattern::Name(name, _),
                        ..
                    }) = function.parameters.get(positional)
                    else {
                        return Err(error("ORNA-EVAL-ARGUMENT"));
                    };
                    positional += 1;
                    name
                };
                if supplied.contains_key(name) {
                    return Err(error("ORNA-EVAL-ARGUMENT"));
                }
                let value = self.evaluate(&argument.value, scope, depth + 1)?;
                supplied.insert(name.clone(), value);
            }
            return invoke_pure(
                self,
                &function.parameters,
                &function.body,
                &function.environment,
                Scope(supplied),
                depth + 1,
            );
        }
        let name = math_name(callee).ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?;
        self.items(arguments.len())?;
        let values = arguments
            .iter()
            .map(|argument| self.evaluate(&argument.value, scope, depth + 1))
            .collect::<Result<Vec<_>, _>>()?;
        self.math(name, named_arguments(name, arguments, values)?)
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
        self.items(arms.len())?;
        for arm in arms {
            let mut local = scope.clone();
            if !bind(&arm.pattern, value.clone(), &mut local, self, depth + 1)? {
                continue;
            }
            if let Some(guard) = &arm.guard {
                match self.evaluate(guard, &mut local, depth + 1)? {
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
            scope.0.insert(name.clone(), value);
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
        }
    }
    Ok(true)
}
fn named_arguments(
    function: &str,
    arguments: &[orna_syntax_v1::Argument],
    values: Vec<Value>,
) -> Result<Vec<Value>, EvaluationError> {
    if arguments.iter().all(|argument| argument.name.is_none()) {
        return Ok(values);
    }
    if arguments.iter().any(|argument| argument.name.is_none()) {
        return Err(error("ORNA-EVAL-UNSUPPORTED"));
    }
    let expected: &[&str] = match function {
        "increment" | "decrement" | "is_zero" => &["value"],
        "min" | "max" => &["left", "right"],
        "clamp" => &["value", "min", "max"],
        _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
    };
    if arguments.len() != expected.len() {
        return Err(error("ORNA-EVAL-UNSUPPORTED"));
    }
    expected
        .iter()
        .map(|expected_name| {
            arguments
                .iter()
                .zip(values.iter())
                .find_map(|(argument, value)| {
                    (argument.name.as_deref() == Some(*expected_name)).then(|| value.clone())
                })
                .ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))
        })
        .collect()
}
fn math_name(expression: &Expr) -> Option<&str> {
    let Expr::Field { base, name, .. } = expression else {
        return None;
    };
    let Expr::Field {
        base, name: module, ..
    } = base.as_ref()
    else {
        return None;
    };
    let Expr::Name { text, .. } = base.as_ref() else {
        return None;
    };
    (text == "std" && module == "math").then_some(name.as_str())
}
fn one_like(value: &Value) -> Result<Value, EvaluationError> {
    match value {
        Value::Int(_) => Ok(Value::Int(1.into())),
        Value::Decimal(_) => DecimalValue::new(1.into(), 0.into()).map(Value::Decimal),
        Value::Float(_) => Ok(Value::Float(1.0f64.to_bits())),
        _ => Err(error("ORNA-EVAL-TYPE")),
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
