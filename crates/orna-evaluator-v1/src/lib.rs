//! A deliberately small, deterministic Orna 1.0 expression evaluator.
//!
//! The public boundary admits and returns only OVB-1 canonical values. It has
//! no I/O, mutation, clock, random, module-loading, or host-call capability.

use std::{collections::BTreeMap, fmt};

use num_bigint::{BigInt, Sign};
use num_integer::Integer;
use num_traits::{Signed, ToPrimitive, Zero};
use orna_foundation_v1::{CanonicalValue, Diagnostic, DiagnosticSeverity, SafeText};
use orna_syntax_v1::{
    ControlKind, Expr, LiteralKind, Pattern, ReplInput, Statement, parse_expression, parse_repl,
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
pub type Environment = BTreeMap<String, CanonicalValue>;

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
    validate_limits(limits)?;
    let mut context = Context { limits, steps: 0 };
    let mut scope = Scope::from_environment(environment, &mut context)?;
    context
        .evaluate(expression, &mut scope, 0)
        .and_then(Value::canonical)
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

struct Context {
    limits: Limits,
    steps: u64,
}
impl Context {
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
        let mut local = scope.clone();
        for statement in statements {
            match statement {
                Statement::Let {
                    pattern,
                    annotation: None,
                    value,
                    ..
                } => {
                    let value = self.evaluate(value, &mut local, depth + 1)?;
                    bind(pattern, value, &mut local, self.limits.max_collection_items)?;
                }
                Statement::Expression { value, .. } => {
                    self.evaluate(value, &mut local, depth + 1)?;
                }
                _ => return Err(error("ORNA-EVAL-UNSUPPORTED")),
            }
        }
        tail.map_or(Ok(Value::Null), |value| {
            self.evaluate(value, &mut local, depth + 1)
        })
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
        let name = math_name(callee).ok_or_else(|| error("ORNA-EVAL-UNSUPPORTED"))?;
        if arguments.iter().any(|argument| argument.name.is_some()) {
            return Err(error("ORNA-EVAL-UNSUPPORTED"));
        }
        self.items(arguments.len())?;
        let values = arguments
            .iter()
            .map(|argument| self.evaluate(&argument.value, scope, depth + 1))
            .collect::<Result<Vec<_>, _>>()?;
        self.math(name, values)
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

fn bind(
    pattern: &Pattern,
    value: Value,
    scope: &mut Scope,
    max_items: usize,
) -> Result<(), EvaluationError> {
    match pattern {
        Pattern::Name(name, _) => {
            scope.0.insert(name.clone(), value);
            Ok(())
        }
        Pattern::Wildcard(_) => Ok(()),
        Pattern::Tuple { elements, .. } => match value {
            Value::Tuple(values) if values.len() == elements.len() && values.len() <= max_items => {
                for (pattern, value) in elements.iter().zip(values) {
                    bind(pattern, value, scope, max_items)?;
                }
                Ok(())
            }
            _ => Err(error("ORNA-EVAL-TYPE")),
        },
        _ => Err(error("ORNA-EVAL-UNSUPPORTED")),
    }
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
    let body = &text[1..text.len() - 1];
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
