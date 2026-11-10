//! Bodies, locals in slots and expressions (design section 2.4).
//!
//! A [`Body`] is one function body, lambda body, initializer or assertion. All
//! names inside it are slots ([`LocalId`]) or definitions ([`DefId`]); a local
//! read is an array index, not a scope-map lookup.

use num_bigint::BigInt;
use orna_syntax::SyntaxSpan as SourceSpan;

use crate::{BodyId, Call, DefId, ExprId, LocalId, Pat, PatId, Ty};

/// One body.
#[derive(Clone, Debug)]
pub struct Body {
    /// The definition whose signature owns this body. A lambda body is owned
    /// by the definition whose body contains the lambda.
    pub owner: DefId,
    /// The expressions, in lowering order.
    pub exprs: crate::IndexVec<ExprId, Expr>,
    /// The patterns, in lowering order.
    pub pats: crate::IndexVec<PatId, Pat>,
    /// One entry per binding. [`LocalId`] is the frame slot.
    pub locals: crate::IndexVec<LocalId, Local>,
    /// The slots `0..n` bound by the argument binder, in parameter order.
    pub params: Box<[LocalId]>,
    /// Only for lambdas: the outer slot each captured slot is copied from.
    /// Capture is by value (VALUE-003).
    pub captures: Box<[Capture]>,
    /// The expression the body returns.
    pub root: ExprId,
    /// The body's own effects.
    pub effects: crate::EffectSet,
}

/// One local binding.
#[derive(Clone, Debug)]
pub struct Local {
    /// Its name, for diagnostics and hover.
    pub name: crate::Name,
    /// Its type.
    pub ty: Ty,
    /// Whether the binding may be assigned.
    pub mutable: bool,
    /// Where it was declared.
    pub span: SourceSpan,
    /// The lexical extent the binding is visible in.
    pub scope: SourceSpan,
}

/// One captured slot of a lambda.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Capture {
    /// The slot in the enclosing body.
    pub outer: LocalId,
    /// The slot in the lambda's own body.
    pub inner: LocalId,
}

/// One expression.
#[derive(Clone, Debug)]
pub struct Expr {
    /// Its structure.
    pub kind: ExprKind,
    /// Its type.
    pub ty: Ty,
    /// Where it was written.
    pub span: SourceSpan,
}

/// The structure of one expression.
#[derive(Clone, Debug)]
pub enum ExprKind {
    /// A literal.
    Lit(Lit),
    /// A read of a local slot.
    Local(LocalId),
    /// A value use of a function, table, variant or module-level binding.
    Def(DefId),
    /// A bare word that named no local or definition; typeck resolves it to a
    /// payload-free variant of the expected enum type, or reports it.
    BareName(crate::Name),
    /// Unary negation or logical not.
    Unary {
        /// The operator.
        op: UnOp,
        /// The operand.
        operand: ExprId,
    },
    /// Arithmetic, comparison or exponentiation.
    Binary {
        /// The operator.
        op: BinOp,
        /// The left operand.
        lhs: ExprId,
        /// The right operand.
        rhs: ExprId,
    },
    /// `&&`, `||` and `??`, which short-circuit.
    Logic {
        /// The operator.
        op: LogicOp,
        /// The left operand.
        lhs: ExprId,
        /// The right operand.
        rhs: ExprId,
    },
    /// A range value.
    Range {
        /// The inclusive lower bound, when written.
        lo: Option<ExprId>,
        /// The upper bound, when written.
        hi: Option<ExprId>,
        /// Whether the upper bound is included.
        inclusive: bool,
    },
    /// A call. A pipe lowers to a call with the piped value as argument 0.
    Call(Box<Call>),
    /// `|?`: run `body`; on failure bind the error in `handler` and run `arm`.
    Recover {
        /// The expression that may fail.
        body: ExprId,
        /// The slot the failure is bound to.
        handler: LocalId,
        /// The recovery expression.
        arm: ExprId,
    },
    /// Field access on a nominal or table value, or a structural record.
    Field {
        /// The base value.
        base: ExprId,
        /// The field.
        field: FieldRef,
    },
    /// Index access.
    Index {
        /// The base value.
        base: ExprId,
        /// The index.
        index: ExprId,
    },
    /// A tuple value.
    Tuple(Box<[ExprId]>),
    /// A list value.
    List(Box<[ExprId]>),
    /// A structural record, fields sorted by name.
    Record(Box<[(crate::Name, ExprId)]>),
    /// A nominal or variant construction, with field definitions.
    Construct {
        /// The constructed definition.
        def: DefId,
        /// The supplied fields, in parameter order.
        fields: Box<[(DefId, ExprId)]>,
    },
    /// A lambda over its own body.
    Lambda {
        /// The lambda body.
        body: BodyId,
        /// The enclosing expressions it captures.
        captured: Box<[ExprId]>,
    },
    /// An interpolated string.
    Interp(Box<[InterpPart]>),
    /// A block of statements and an optional tail expression.
    Block {
        /// The statements.
        stmts: Box<[Stmt]>,
        /// The tail expression, when the block has one.
        tail: Option<ExprId>,
    },
    /// `if`.
    If {
        /// The condition.
        cond: ExprId,
        /// The branch taken when the condition holds.
        then: ExprId,
        /// The `else` branch, when written.
        els: Option<ExprId>,
    },
    /// `case`.
    Case {
        /// The scrutinee.
        scrutinee: ExprId,
        /// The arms, in source order.
        arms: Box<[Arm]>,
    },
    /// `for`.
    For {
        /// The loop binding.
        pat: PatId,
        /// The value iterated.
        iter: ExprId,
        /// The loop body.
        body: ExprId,
    },
    /// `while`.
    While {
        /// The condition.
        cond: ExprId,
        /// The loop body.
        body: ExprId,
    },
    /// `loop`.
    Loop {
        /// The loop body.
        body: ExprId,
    },
    /// `return`, with an optional value.
    Return(Option<ExprId>),
    /// `break`, with an optional value.
    Break(Option<ExprId>),
    /// `continue`.
    Continue,
    /// An assignment to a place.
    Assign {
        /// The place assigned.
        place: Place,
        /// The assignment operator.
        op: AssignOp,
        /// The assigned value.
        value: ExprId,
    },
    /// A diagnostic was emitted for this expression.
    Error,
}

/// A literal. Payloads are the canonical scalar encodings, so no runtime
/// [`orna_value::Value`] enters the HIR.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Lit {
    /// An integer of arbitrary width.
    Int(BigInt),
    /// A decimal as its canonical coefficient and base-ten exponent.
    Decimal {
        /// The coefficient.
        coefficient: BigInt,
        /// The base-ten exponent.
        exponent: BigInt,
    },
    /// A binary64 value, as its bit pattern.
    Float(u64),
    /// A string, already unescaped.
    Str(Box<str>),
    /// A boolean.
    Bool(bool),
    /// The absent option value, `null`. Its payload type is inferred at the
    /// use (design section 2.3, D3), so the literal carries no payload.
    None,
    /// A date, as days since the epoch.
    Date(i64),
    /// An instant, as seconds since the epoch and a nanoseconds component.
    Instant {
        /// Seconds since the epoch.
        seconds: i64,
        /// The nanoseconds within the second.
        nanoseconds: u32,
    },
    /// A period of whole days, from `N.day` or `N.days`.
    Period(BigInt),
    /// An elapsed duration as whole seconds and a nanoseconds component, from
    /// a built-in duration postfix such as `3.hour` or `1.5.second`.
    Duration {
        /// Whole seconds.
        seconds: BigInt,
        /// The nanoseconds within the second, below 1_000_000_000.
        nanoseconds: u32,
    },
}

/// A unary operator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnOp {
    /// `-`.
    Neg,
    /// `!`.
    Not,
}

/// A binary arithmetic, comparison or power operator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinOp {
    /// `+`.
    Add,
    /// `-`.
    Sub,
    /// `*`.
    Mul,
    /// `/`.
    Div,
    /// `%`.
    Rem,
    /// `^`.
    Pow,
    /// `==`.
    Eq,
    /// `!=`.
    Ne,
    /// `<`.
    Lt,
    /// `<=`.
    Le,
    /// `>`.
    Gt,
    /// `>=`.
    Ge,
}

/// A short-circuiting logical operator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogicOp {
    /// `&&`.
    And,
    /// `||`.
    Or,
    /// `??`.
    Coalesce,
}

/// A field reference: a declared field, a structural record position, or a
/// member name the type pass settles.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FieldRef {
    /// A field definition of a nominal type, table or enum payload.
    Def(DefId),
    /// The position of a field in a structural record, sorted by name.
    Index(u32),
    /// A member name whose field definition depends on the base's type. The
    /// type pass of stage H2 replaces it with [`FieldRef::Def`] or
    /// [`FieldRef::Index`]; until then it is the only total representation of
    /// `row.field` that keeps the written name.
    Name(crate::Name),
}

/// A place an assignment can write.
#[derive(Clone, Debug)]
pub enum Place {
    /// A local slot.
    Local(LocalId),
    /// A field of a place.
    Field {
        /// The base place.
        base: Box<Place>,
        /// The field.
        field: FieldRef,
    },
    /// An index of a place.
    Index {
        /// The base place.
        base: Box<Place>,
        /// The index expression.
        index: ExprId,
    },
}

/// An assignment operator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssignOp {
    /// `=`.
    Set,
    /// `+=`.
    Add,
    /// `-=`.
    Subtract,
    /// `*=`.
    Multiply,
    /// `/=`.
    Divide,
}

/// One statement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stmt {
    /// `let` with a pattern and an initializer.
    Let {
        /// The binding pattern.
        pat: PatId,
        /// The initializer.
        init: ExprId,
    },
    /// An assertion expression.
    Assert(ExprId),
    /// An expression evaluated for its effects.
    Expr(ExprId),
}

/// One part of an interpolated string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InterpPart {
    /// Literal text.
    Text(Box<str>),
    /// An interpolated expression.
    Expr(ExprId),
}

/// One `case` arm.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Arm {
    /// The pattern.
    pub pat: PatId,
    /// The guard, when written.
    pub guard: Option<ExprId>,
    /// The arm body.
    pub body: ExprId,
}
