//! The syntax tree: the item, declaration, type, expression and pattern nodes produced by the parser.

use crate::interface::{InterfaceClauses, UnitExpr};
use crate::span::SourceSpan;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxTree {
    pub entry: EntryPoint,
    pub items: Vec<Item>,
    pub span: SourceSpan,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub kind: ItemKind,
    pub visibility: Visibility,
    pub span: SourceSpan,
    /// The `///` doc comment lines written directly above the item, without the marker.
    pub docs: Vec<String>,
    /// Interface clauses (`since`, `removed`, ...). Empty for every module outside the engine's interface.
    pub clauses: InterfaceClauses,
    pub declaration: Declaration,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Visibility {
    Public { span: SourceSpan },
    Private,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Declaration {
    Use {
        path: Vec<ImportSegment>,
        tail: UseTail,
    },
    Table {
        name: String,
        keys: Vec<Parameter>,
        members: Vec<TableMember>,
    },
    Function {
        signature: FunctionSignature,
        body: Expr,
    },
    /// `pub native fn name(parameter: Type = default): Result effect classes;`
    ///
    /// A native declaration states that the admitted pinned dependency supplies
    /// the implementation, so it has no body and no body field: every consumer
    /// that reads a function body must name this variant explicitly
    /// (ORNA-NATIVE-001).
    Native {
        signature: FunctionSignature,
        /// The mandatory effect clause. `None` is a recovered parse: the clause
        /// was absent or malformed and ORNA091-E-NATIVE-EFFECT was reported.
        effects: Option<NativeEffectSet>,
    },
    /// `pub native type Name<T>;` declares an opaque nominal type that the host supplies (SN1).
    NativeType {
        name: String,
        generics: Vec<GenericParameter>,
        span: SourceSpan,
    },
    /// `pub native static name: T effect set;` declares a host-supplied singleton value (SN1).
    NativeStatic {
        name: String,
        annotation: Option<TypeExpr>,
        effects: Option<NativeEffectSet>,
        span: SourceSpan,
    },
    Protocol {
        name: String,
        generics: Vec<GenericParameter>,
        members: Vec<ProtocolMember>,
    },
    Enum {
        name: String,
        generics: Vec<GenericParameter>,
        variants: Vec<EnumVariant>,
        implementations: Vec<Implementation>,
        /// Whether the enum is a unit family, marked by a trailing `impl Unit {}`.
        unit_family: bool,
    },
    Dimension {
        name: String,
        expression: Option<DimensionExpr>,
    },
    Unit {
        name: String,
        dimension: TypeExpr,
        definition: UnitDefinition,
    },
    Type {
        name: String,
        generics: Vec<GenericParameter>,
        representation: TypeRepresentation,
    },
    Assertion {
        value: Expr,
    },
    Let {
        pattern: Pattern,
        annotation: Option<TypeExpr>,
        value: Expr,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UseTail {
    None,
    Alias { name: String, span: SourceSpan },
    Glob { span: SourceSpan },
    Names(Vec<ImportSegment>),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSegment {
    pub name: String,
    pub span: SourceSpan,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenericParameter {
    pub name: String,
    pub bounds: Vec<TypeExpr>,
    pub span: SourceSpan,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    pub pattern: Pattern,
    pub annotation: Option<TypeExpr>,
    pub default: Option<Expr>,
    pub span: SourceSpan,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionSignature {
    pub name: String,
    pub generics: Vec<GenericParameter>,
    pub parameters: Vec<Parameter>,
    pub result: Option<TypeExpr>,
    pub span: SourceSpan,
}
/// The spellings of the closed `sys.EffectKind` vocabulary, in declaration
/// order. A native declaration's effect clause names one or more of these or
/// the `none` spelling of the empty set (ORNA-NATIVE-009).
///
/// Recognition lives here; classification lives in `orna_check`'s
/// `effects::EffectKind`, whose spellings these are. Stage N3/N5 unify the two
/// when the checker consumes the clause.
pub const EFFECT_CLASSES: [&str; 12] = [
    "table_read",
    "table_write",
    "network",
    "filesystem",
    "process",
    "clock",
    "randomness",
    "secret_access",
    "ui",
    "git",
    "storage_admin",
    "runtime_admin",
];
/// The mandatory effect clause of a native declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeEffectSet {
    /// The `none` spelling: the empty host-effect set.
    None { span: SourceSpan },
    /// One or more `sys.EffectKind` class names, in source order.
    Classes {
        classes: Vec<NameSegment>,
        span: SourceSpan,
    },
}
impl NativeEffectSet {
    pub fn span(&self) -> &SourceSpan {
        match self {
            Self::None { span } | Self::Classes { span, .. } => span,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldInitializer {
    Default(Expr),
    Computed(Expr),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableMember {
    Field {
        name: String,
        ty: TypeExpr,
        initializer: Option<FieldInitializer>,
        span: SourceSpan,
    },
    Assertion {
        value: Expr,
        span: SourceSpan,
    },
    Implementation {
        implementation: Implementation,
        span: SourceSpan,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolMember {
    Function {
        signature: FunctionSignature,
        span: SourceSpan,
    },
    Static {
        name: String,
        ty: TypeExpr,
        span: SourceSpan,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumVariant {
    pub name: String,
    pub fields: Vec<EnumPayloadField>,
    /// The `= unit_expr` of a unit variant; `None` for a payload variant and for the base unit.
    pub unit: Option<UnitExpr>,
    pub span: SourceSpan,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumPayloadField {
    pub name: String,
    pub ty: TypeExpr,
    pub span: SourceSpan,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeMember {
    Field {
        visibility: bool,
        name: String,
        ty: TypeExpr,
        initializer: Option<Expr>,
        span: SourceSpan,
    },
    Assertion {
        value: Expr,
        span: SourceSpan,
    },
    Implementation {
        implementation: Implementation,
        span: SourceSpan,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRepresentation {
    Alias {
        ty: TypeExpr,
        refinements: Vec<TypeMember>,
    },
    Nominal {
        members: Vec<TypeMember>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Implementation {
    pub protocol: TypeExpr,
    pub members: Vec<ImplMember>,
    pub span: SourceSpan,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImplMember {
    Function {
        signature: FunctionSignature,
        body: Expr,
        span: SourceSpan,
    },
    Static {
        name: String,
        ty: Option<TypeExpr>,
        value: Expr,
        span: SourceSpan,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DimensionExpr {
    pub terms: Vec<(String, TypeExpr, Option<String>, SourceSpan)>,
    pub operators: Vec<String>,
    pub span: SourceSpan,
}
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnitDefinition {
    Base,
    Derived {
        value: Expr,
        offset: Option<Expr>,
        affine: bool,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeExpr {
    Name {
        path: Vec<String>,
        arguments: Vec<TypeExpr>,
        span: SourceSpan,
    },
    Optional {
        inner: Box<TypeExpr>,
        span: SourceSpan,
    },
    Product {
        lhs: Box<TypeExpr>,
        op: String,
        rhs: Box<TypeExpr>,
        span: SourceSpan,
    },
    List {
        inner: Box<TypeExpr>,
        span: SourceSpan,
    },
    Record {
        fields: Vec<(String, TypeExpr, SourceSpan)>,
        span: SourceSpan,
    },
    Tuple {
        elements: Vec<TypeExpr>,
        span: SourceSpan,
    },
    Function {
        parameters: Vec<TypeExpr>,
        result: Box<TypeExpr>,
        span: SourceSpan,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Use,
    Table,
    Function,
    Native,
    Protocol,
    Enum,
    Dimension,
    Unit,
    Type,
    Assert,
    Let,
    Expression,
}
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplInput {
    Item(Item),
    Expression(Expr),
    ExpressionStatement(Expr),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Name {
        text: String,
        span: SourceSpan,
    },
    Literal {
        text: String,
        kind: LiteralKind,
        span: SourceSpan,
    },
    InterpolatedString {
        segments: Vec<StringSegment>,
        span: SourceSpan,
    },
    ReplBinding {
        text: String,
        span: SourceSpan,
    },
    Unary {
        op: String,
        rhs: Box<Expr>,
        span: SourceSpan,
    },
    Binary {
        lhs: Box<Expr>,
        op: String,
        rhs: Box<Expr>,
        span: SourceSpan,
    },
    Range {
        lower: Option<Box<Expr>>,
        operator: String,
        upper: Option<Box<Expr>>,
        span: SourceSpan,
    },
    Call {
        callee: Box<Expr>,
        arguments: Vec<Argument>,
        span: SourceSpan,
    },
    GenericCall {
        callee: Box<Expr>,
        type_arguments: Vec<TypeExpr>,
        arguments: Vec<Argument>,
        span: SourceSpan,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
        span: SourceSpan,
    },
    Field {
        base: Box<Expr>,
        name: String,
        span: SourceSpan,
    },
    Group {
        inner: Box<Expr>,
        span: SourceSpan,
    },
    Tuple {
        elements: Vec<Expr>,
        span: SourceSpan,
    },
    Record {
        fields: Vec<RecordField>,
        span: SourceSpan,
    },
    Nominal {
        path: Vec<NameSegment>,
        fields: Vec<RecordField>,
        span: SourceSpan,
    },
    List {
        elements: Vec<Expr>,
        span: SourceSpan,
    },
    Lambda {
        parameters: Vec<LambdaParameter>,
        body: Box<Expr>,
        span: SourceSpan,
    },
    Block {
        statements: Vec<Statement>,
        tail: Option<Box<Expr>>,
        span: SourceSpan,
    },
    Control {
        kind: ControlKind,
        binding: Option<Pattern>,
        condition: Option<Box<Expr>>,
        body: Option<Box<Expr>>,
        arms: Vec<CaseArm>,
        alternate: Option<Box<Expr>>,
        span: SourceSpan,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameSegment {
    pub text: String,
    pub span: SourceSpan,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LambdaParameter {
    pub pattern: Pattern,
    pub annotation: Option<TypeExpr>,
    pub span: SourceSpan,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiteralKind {
    Integer,
    Decimal,
    Float,
    Date,
    Instant,
    String,
    Boolean,
    Null,
}
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum StringSegment {
    Text { text: String, span: SourceSpan },
    Expression { value: Expr, span: SourceSpan },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Statement {
    Let {
        pattern: Pattern,
        annotation: Option<TypeExpr>,
        value: Expr,
        span: SourceSpan,
    },
    Assert {
        value: Expr,
        span: SourceSpan,
    },
    Return {
        value: Option<Expr>,
        span: SourceSpan,
    },
    Break {
        value: Option<Expr>,
        span: SourceSpan,
    },
    Continue {
        span: SourceSpan,
    },
    Assignment {
        target: AssignmentTarget,
        operator: AssignmentOperator,
        value: Expr,
        span: SourceSpan,
    },
    Expression {
        value: Expr,
        span: SourceSpan,
    },
    Control {
        value: Expr,
        span: SourceSpan,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignmentOperator {
    Set,
    Add,
    Subtract,
    Multiply,
    Divide,
}
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum AssignmentTarget {
    Name {
        name: String,
        span: SourceSpan,
    },
    Field {
        base: Box<AssignmentTarget>,
        name: String,
        span: SourceSpan,
    },
    Index {
        base: Box<AssignmentTarget>,
        index: Expr,
        span: SourceSpan,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Argument {
    pub name: Option<String>,
    pub value: Expr,
    pub span: SourceSpan,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordField {
    pub name: String,
    pub value: Expr,
    pub span: SourceSpan,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern {
    Name(String, SourceSpan),
    Wildcard(SourceSpan),
    Literal {
        text: String,
        kind: LiteralKind,
        span: SourceSpan,
    },
    Tuple {
        elements: Vec<Pattern>,
        span: SourceSpan,
    },
    List {
        elements: Vec<Pattern>,
        span: SourceSpan,
    },
    Record {
        fields: Vec<(String, Option<Pattern>, SourceSpan)>,
        span: SourceSpan,
    },
    Constructor {
        path: Vec<NameSegment>,
        arguments: Vec<Pattern>,
        fields: Vec<PatternField>,
        span: SourceSpan,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternField {
    pub name: String,
    pub pattern: Option<Pattern>,
    pub span: SourceSpan,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlKind {
    If,
    Case,
    For,
    While,
    Loop,
}
impl Pattern {
    /// The source range of the whole pattern.
    pub fn span(&self) -> &SourceSpan {
        match self {
            Self::Name(_, span)
            | Self::Wildcard(span)
            | Self::Literal { span, .. }
            | Self::Tuple { span, .. }
            | Self::List { span, .. }
            | Self::Record { span, .. }
            | Self::Constructor { span, .. } => span,
        }
    }
}
impl AssignmentTarget {
    /// The source range of the whole target.
    pub fn span(&self) -> &SourceSpan {
        match self {
            Self::Name { span, .. } | Self::Field { span, .. } | Self::Index { span, .. } => span,
        }
    }
}
impl TypeExpr {
    /// The source range of the whole type expression.
    pub fn span(&self) -> &SourceSpan {
        match self {
            Self::Name { span, .. }
            | Self::Optional { span, .. }
            | Self::Product { span, .. }
            | Self::List { span, .. }
            | Self::Record { span, .. }
            | Self::Tuple { span, .. }
            | Self::Function { span, .. } => span,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseArm {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Expr,
    pub span: SourceSpan,
}
impl Expr {
    pub fn span(&self) -> SourceSpan {
        match self {
            Self::Name { span, .. }
            | Self::Literal { span, .. }
            | Self::InterpolatedString { span, .. }
            | Self::ReplBinding { span, .. }
            | Self::Unary { span, .. }
            | Self::Binary { span, .. }
            | Self::Range { span, .. }
            | Self::Call { span, .. }
            | Self::GenericCall { span, .. }
            | Self::Field { span, .. }
            | Self::Group { span, .. }
            | Self::Tuple { span, .. }
            | Self::Record { span, .. }
            | Self::Nominal { span, .. }
            | Self::List { span, .. }
            | Self::Lambda { span, .. }
            | Self::Block { span, .. }
            | Self::Control { span, .. }
            | Self::Index { span, .. } => span.clone(),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryPoint {
    Module,
    Row,
    Repl,
}
