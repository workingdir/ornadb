use crate::lexer::{Keyword, LexError, Token, TokenKind, lex};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourcePosition {
    pub line: u32,
    pub column: u32,
}
/// Half-open UTF-8 bytes. Locations are optional for unsaved editor buffers;
/// when populated they use 1-based Unicode-scalar line/column coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceSpan {
    pub start: usize,
    pub end: usize,
    pub file: Option<String>,
    pub start_position: Option<SourcePosition>,
    pub end_position: Option<SourcePosition>,
}
impl SourceSpan {
    pub const fn new(start: usize, end: usize) -> Self {
        Self {
            start,
            end,
            file: None,
            start_position: None,
            end_position: None,
        }
    }
    pub fn join(self, other: Self) -> Self {
        if self.file == other.file {
            Self {
                start: self.start,
                end: other.end,
                file: self.file,
                start_position: self.start_position,
                end_position: other.end_position,
            }
        } else {
            Self::new(self.start, other.end)
        }
    }
    pub fn located(mut self, file: impl Into<String>, source: &str) -> Self {
        self.file = Some(file.into());
        self.start_position = Some(position_at(source, self.start));
        self.end_position = Some(position_at(source, self.end));
        self
    }
}
fn position_at(source: &str, at: usize) -> SourcePosition {
    let before = &source[..at.min(source.len())];
    SourcePosition {
        line: before.bytes().filter(|b| *b == b'\n').count() as u32 + 1,
        column: before.rsplit('\n').next().unwrap_or("").chars().count() as u32 + 1,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub span: SourceSpan,
    pub message: Option<String>,
    pub primary: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub title: String,
    pub message: String,
    pub span: SourceSpan,
    pub labels: Vec<Label>,
    pub help: Vec<String>,
    pub notes: Vec<String>,
    pub causes: Vec<Box<Diagnostic>>,
    pub safe_data: Vec<(String, String)>,
    pub redacted: bool,
    pub trace_id: Option<String>,
}
impl Diagnostic {
    fn error(code: &'static str, message: impl Into<String>, span: SourceSpan) -> Self {
        let message = message.into();
        Self {
            code,
            title: message.clone(),
            message,
            span,
            labels: Vec::new(),
            help: Vec::new(),
            notes: Vec::new(),
            causes: Vec::new(),
            safe_data: Vec::new(),
            redacted: false,
            trace_id: None,
        }
    }
}
pub type ParseError = Diagnostic;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryPoint {
    Module,
    Row,
    Repl,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parse<T> {
    pub value: T,
    pub diagnostics: Vec<Diagnostic>,
}
impl<T> Parse<T> {
    pub fn is_ok(&self) -> bool {
        self.diagnostics.is_empty()
    }
}
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
    Protocol {
        name: String,
        generics: Vec<GenericParameter>,
        members: Vec<ProtocolMember>,
    },
    Enum {
        name: String,
        generics: Vec<GenericParameter>,
        variants: Vec<EnumVariant>,
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
    Call {
        callee: Box<Expr>,
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
fn pattern_span(pattern: &Pattern) -> SourceSpan {
    match pattern {
        Pattern::Name(_, span) | Pattern::Wildcard(span) | Pattern::Literal { span, .. } => {
            span.clone()
        }
        Pattern::Tuple { span, .. }
        | Pattern::List { span, .. }
        | Pattern::Record { span, .. }
        | Pattern::Constructor { span, .. } => span.clone(),
    }
}
fn assignment_target_span(target: &AssignmentTarget) -> SourceSpan {
    match target {
        AssignmentTarget::Name { span, .. }
        | AssignmentTarget::Field { span, .. }
        | AssignmentTarget::Index { span, .. } => span.clone(),
    }
}
#[allow(dead_code)]
fn type_span(ty: &TypeExpr) -> SourceSpan {
    match ty {
        TypeExpr::Name { span, .. }
        | TypeExpr::Optional { span, .. }
        | TypeExpr::Product { span, .. }
        | TypeExpr::List { span, .. }
        | TypeExpr::Record { span, .. }
        | TypeExpr::Tuple { span, .. }
        | TypeExpr::Function { span, .. } => span.clone(),
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
            | Self::Call { span, .. }
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

pub fn parse_module(source: &str) -> Parse<SyntaxTree> {
    let mut p = Parser::new(source);
    p.validate_token_grammar();
    let mut items = Vec::new();
    while !p.eof() {
        if let Some(item) = p.item(true) {
            items.push(item)
        } else {
            p.recover_top()
        }
    }
    let span = SourceSpan::new(0, source.len());
    Parse {
        value: SyntaxTree {
            entry: EntryPoint::Module,
            items,
            span,
        },
        diagnostics: p.errors,
    }
}
pub fn parse_row(source: &str) -> Parse<Expr> {
    let mut p = Parser::new(source);
    let result = if p.is_punct("{") {
        p.expr()
    } else {
        p.error_here("E8001", "row unit must contain one record expression");
        None
    };
    if p.is_punct(";") {
        p.bump()
    }
    p.finish();
    Parse {
        value: result.unwrap_or(Expr::Record {
            fields: Vec::new(),
            span: SourceSpan::new(0, 0),
        }),
        diagnostics: p.errors,
    }
}
pub fn parse_repl(source: &str) -> Parse<ReplInput> {
    let mut p = Parser::new(source);
    let value = if matches!(
        p.keyword(),
        Some(Keyword::Use | Keyword::Let | Keyword::Fn | Keyword::Pub)
    ) {
        p.item(false).map(ReplInput::Item)
    } else {
        p.expr().map(ReplInput::Expression)
    };
    p.finish();
    Parse {
        value: value.unwrap_or(ReplInput::Expression(Expr::Tuple {
            elements: Vec::new(),
            span: SourceSpan::new(0, 0),
        })),
        diagnostics: p.errors,
    }
}
pub fn parse_expression(source: &str) -> Parse<Expr> {
    let mut p = Parser::new(source);
    let v = p.expr();
    p.finish();
    Parse {
        value: v.unwrap_or(Expr::Tuple {
            elements: Vec::new(),
            span: SourceSpan::new(0, 0),
        }),
        diagnostics: p.errors,
    }
}
pub fn parse_module_with_file(source: &str, file: impl Into<String>) -> Parse<SyntaxTree> {
    let mut parsed = parse_module(source);
    let file = file.into();
    annotate_tree(&mut parsed.value, source, &file);
    annotate_diagnostics(&mut parsed.diagnostics, source, &file);
    parsed
}
pub fn parse_row_with_file(source: &str, file: impl Into<String>) -> Parse<Expr> {
    let mut parsed = parse_row(source);
    let file = file.into();
    annotate_expr(&mut parsed.value, source, &file);
    annotate_diagnostics(&mut parsed.diagnostics, source, &file);
    parsed
}
pub fn parse_repl_with_file(source: &str, file: impl Into<String>) -> Parse<ReplInput> {
    let mut parsed = parse_repl(source);
    let file = file.into();
    match &mut parsed.value {
        ReplInput::Item(item) => annotate_item(item, source, &file),
        ReplInput::Expression(expr) => annotate_expr(expr, source, &file),
    }
    annotate_diagnostics(&mut parsed.diagnostics, source, &file);
    parsed
}
pub fn parse_expression_with_file(source: &str, file: impl Into<String>) -> Parse<Expr> {
    let mut parsed = parse_expression(source);
    let file = file.into();
    annotate_expr(&mut parsed.value, source, &file);
    annotate_diagnostics(&mut parsed.diagnostics, source, &file);
    parsed
}

fn annotate_span(span: &mut SourceSpan, source: &str, file: &str) {
    *span = span.clone().located(file, source)
}
fn annotate_item(item: &mut Item, source: &str, file: &str) {
    if let Visibility::Public { span } = &mut item.visibility {
        annotate_span(span, source, file);
    }
    match &mut item.declaration {
        Declaration::Function { signature, body } => {
            annotate_signature(signature, source, file);
            annotate_expr(body, source, file)
        }
        Declaration::Table { keys, members, .. } => {
            for key in keys {
                annotate_parameter(key, source, file);
            }
            for member in members {
                match member {
                    TableMember::Field {
                        ty,
                        initializer,
                        span,
                        ..
                    } => {
                        annotate_type(ty, source, file);
                        if let Some(initializer) = initializer {
                            match initializer {
                                FieldInitializer::Default(value)
                                | FieldInitializer::Computed(value) => {
                                    annotate_expr(value, source, file)
                                }
                            }
                        }
                        annotate_span(span, source, file)
                    }
                    TableMember::Assertion { value, span } => {
                        annotate_expr(value, source, file);
                        annotate_span(span, source, file)
                    }
                    TableMember::Implementation {
                        implementation,
                        span,
                    } => {
                        annotate_implementation(implementation, source, file);
                        annotate_span(span, source, file)
                    }
                }
            }
        }
        Declaration::Protocol {
            generics, members, ..
        } => {
            for parameter in generics {
                annotate_generic(parameter, source, file);
            }
            for member in members {
                match member {
                    ProtocolMember::Function { signature, span } => {
                        annotate_signature(signature, source, file);
                        annotate_span(span, source, file)
                    }
                    ProtocolMember::Static { ty, span, .. } => {
                        annotate_type(ty, source, file);
                        annotate_span(span, source, file)
                    }
                }
            }
        }
        Declaration::Enum {
            generics, variants, ..
        } => {
            for parameter in generics {
                annotate_generic(parameter, source, file);
            }
            for variant in variants {
                for field in &mut variant.fields {
                    annotate_type(&mut field.ty, source, file);
                    annotate_span(&mut field.span, source, file);
                }
                annotate_span(&mut variant.span, source, file)
            }
        }
        Declaration::Type {
            generics,
            representation,
            ..
        } => {
            for parameter in generics {
                annotate_generic(parameter, source, file);
            }
            match representation {
                TypeRepresentation::Alias { ty, refinements } => {
                    annotate_type(ty, source, file);
                    for member in refinements {
                        annotate_type_member(member, source, file)
                    }
                }
                TypeRepresentation::Nominal { members } => {
                    for member in members {
                        annotate_type_member(member, source, file)
                    }
                }
            }
        }
        Declaration::Dimension { expression, .. } => {
            if let Some(expression) = expression {
                for (_, ty, _, span) in &mut expression.terms {
                    annotate_type(ty, source, file);
                    annotate_span(span, source, file);
                }
                annotate_span(&mut expression.span, source, file);
            }
        }
        Declaration::Unit {
            dimension,
            definition,
            ..
        } => {
            annotate_type(dimension, source, file);
            if let UnitDefinition::Derived { value, offset, .. } = definition {
                annotate_expr(value, source, file);
                if let Some(offset) = offset {
                    annotate_expr(offset, source, file)
                }
            }
        }
        Declaration::Assertion { value } => annotate_expr(value, source, file),
        Declaration::Let {
            pattern,
            annotation,
            value,
        } => {
            annotate_pattern(pattern, source, file);
            if let Some(annotation) = annotation {
                annotate_type(annotation, source, file)
            };
            annotate_expr(value, source, file)
        }
        Declaration::Use { path, tail } => {
            for segment in path {
                annotate_span(&mut segment.span, source, file)
            }
            match tail {
                UseTail::None => {}
                UseTail::Alias { span, .. } | UseTail::Glob { span } => {
                    annotate_span(span, source, file)
                }
                UseTail::Names(names) => {
                    for segment in names {
                        annotate_span(&mut segment.span, source, file)
                    }
                }
            }
        }
    }
    annotate_span(&mut item.span, source, file)
}
fn annotate_pattern(pattern: &mut Pattern, source: &str, file: &str) {
    match pattern {
        Pattern::Name(_, span) | Pattern::Wildcard(span) | Pattern::Literal { span, .. } => {
            annotate_span(span, source, file)
        }
        Pattern::Tuple { elements, span }
        | Pattern::List { elements, span }
        | Pattern::Constructor {
            arguments: elements,
            span,
            ..
        } => {
            for element in elements {
                annotate_pattern(element, source, file)
            }
            annotate_span(span, source, file)
        }
        Pattern::Record { fields, span } => {
            for (_, pattern, field_span) in fields {
                if let Some(pattern) = pattern {
                    annotate_pattern(pattern, source, file)
                }
                annotate_span(field_span, source, file)
            }
            annotate_span(span, source, file)
        }
    }
}
fn annotate_type(ty: &mut TypeExpr, source: &str, file: &str) {
    match ty {
        TypeExpr::Name {
            arguments, span, ..
        } => {
            for argument in arguments {
                annotate_type(argument, source, file)
            }
            annotate_span(span, source, file)
        }
        TypeExpr::Optional { inner, span } | TypeExpr::List { inner, span } => {
            annotate_type(inner, source, file);
            annotate_span(span, source, file)
        }
        TypeExpr::Product { lhs, rhs, span, .. } => {
            annotate_type(lhs, source, file);
            annotate_type(rhs, source, file);
            annotate_span(span, source, file)
        }
        TypeExpr::Record { fields, span } => {
            for (_, ty, field_span) in fields {
                annotate_type(ty, source, file);
                annotate_span(field_span, source, file)
            }
            annotate_span(span, source, file)
        }
        TypeExpr::Tuple { elements, span } => {
            for element in elements {
                annotate_type(element, source, file)
            }
            annotate_span(span, source, file)
        }
        TypeExpr::Function {
            parameters,
            result,
            span,
        } => {
            for parameter in parameters {
                annotate_type(parameter, source, file)
            }
            annotate_type(result, source, file);
            annotate_span(span, source, file)
        }
    }
}
fn annotate_generic(parameter: &mut GenericParameter, source: &str, file: &str) {
    for bound in &mut parameter.bounds {
        annotate_type(bound, source, file)
    }
    annotate_span(&mut parameter.span, source, file)
}
fn annotate_parameter(parameter: &mut Parameter, source: &str, file: &str) {
    annotate_pattern(&mut parameter.pattern, source, file);
    if let Some(annotation) = &mut parameter.annotation {
        annotate_type(annotation, source, file)
    };
    if let Some(default) = &mut parameter.default {
        annotate_expr(default, source, file)
    };
    annotate_span(&mut parameter.span, source, file)
}
fn annotate_signature(signature: &mut FunctionSignature, source: &str, file: &str) {
    for generic in &mut signature.generics {
        annotate_generic(generic, source, file)
    }
    for parameter in &mut signature.parameters {
        annotate_parameter(parameter, source, file)
    }
    if let Some(result) = &mut signature.result {
        annotate_type(result, source, file)
    }
    annotate_span(&mut signature.span, source, file)
}
fn annotate_implementation(implementation: &mut Implementation, source: &str, file: &str) {
    annotate_type(&mut implementation.protocol, source, file);
    for member in &mut implementation.members {
        match member {
            ImplMember::Function {
                signature,
                body,
                span,
            } => {
                annotate_signature(signature, source, file);
                annotate_expr(body, source, file);
                annotate_span(span, source, file)
            }
            ImplMember::Static {
                ty, value, span, ..
            } => {
                if let Some(ty) = ty {
                    annotate_type(ty, source, file)
                };
                annotate_expr(value, source, file);
                annotate_span(span, source, file)
            }
        }
    }
    annotate_span(&mut implementation.span, source, file)
}
fn annotate_type_member(member: &mut TypeMember, source: &str, file: &str) {
    match member {
        TypeMember::Field {
            ty,
            initializer,
            span,
            ..
        } => {
            annotate_type(ty, source, file);
            if let Some(initializer) = initializer {
                annotate_expr(initializer, source, file)
            };
            annotate_span(span, source, file)
        }
        TypeMember::Assertion { value, span } => {
            annotate_expr(value, source, file);
            annotate_span(span, source, file)
        }
        TypeMember::Implementation {
            implementation,
            span,
        } => {
            annotate_implementation(implementation, source, file);
            annotate_span(span, source, file)
        }
    }
}
fn annotate_tree(tree: &mut SyntaxTree, source: &str, file: &str) {
    annotate_span(&mut tree.span, source, file);
    for item in &mut tree.items {
        annotate_item(item, source, file)
    }
}
fn annotate_expr(expr: &mut Expr, source: &str, file: &str) {
    match expr {
        Expr::Unary { rhs, span, .. } => {
            annotate_expr(rhs, source, file);
            annotate_span(span, source, file)
        }
        Expr::Binary { lhs, rhs, span, .. } => {
            annotate_expr(lhs, source, file);
            annotate_expr(rhs, source, file);
            annotate_span(span, source, file)
        }
        Expr::Call { callee, span, .. } => {
            annotate_expr(callee, source, file);
            annotate_span(span, source, file)
        }
        Expr::Field { base, span, .. } => {
            annotate_expr(base, source, file);
            annotate_span(span, source, file)
        }
        Expr::Index { base, index, span } => {
            annotate_expr(base, source, file);
            annotate_expr(index, source, file);
            annotate_span(span, source, file)
        }
        Expr::Tuple { elements, span } | Expr::List { elements, span } => {
            for element in elements {
                annotate_expr(element, source, file)
            }
            annotate_span(span, source, file)
        }
        Expr::Record { fields, span } => {
            for field in fields {
                annotate_expr(&mut field.value, source, file);
                annotate_span(&mut field.span, source, file)
            }
            annotate_span(span, source, file)
        }
        Expr::Nominal { fields, span, .. } => {
            for field in fields {
                annotate_expr(&mut field.value, source, file);
                annotate_span(&mut field.span, source, file)
            }
            annotate_span(span, source, file)
        }
        Expr::Lambda { body, span, .. } => {
            annotate_expr(body, source, file);
            annotate_span(span, source, file)
        }
        Expr::Block {
            statements,
            tail,
            span,
        } => {
            for statement in statements {
                annotate_statement(statement, source, file);
            }
            if let Some(tail) = tail {
                annotate_expr(tail, source, file)
            }
            annotate_span(span, source, file)
        }
        Expr::Control {
            condition,
            body,
            arms,
            alternate,
            span,
            ..
        } => {
            if let Some(condition) = condition {
                annotate_expr(condition, source, file)
            }
            if let Some(body) = body {
                annotate_expr(body, source, file)
            }
            for arm in arms {
                if let Some(guard) = &mut arm.guard {
                    annotate_expr(guard, source, file)
                };
                annotate_expr(&mut arm.body, source, file);
                annotate_span(&mut arm.span, source, file)
            }
            if let Some(alternate) = alternate {
                annotate_expr(alternate, source, file)
            }
            annotate_span(span, source, file)
        }
        Expr::Group { inner, span } => {
            annotate_expr(inner, source, file);
            annotate_span(span, source, file)
        }
        Expr::Name { span, .. } | Expr::Literal { span, .. } | Expr::ReplBinding { span, .. } => {
            annotate_span(span, source, file)
        }
        Expr::InterpolatedString { segments, span } => {
            for segment in segments {
                match segment {
                    StringSegment::Text { span, .. } => annotate_span(span, source, file),
                    StringSegment::Expression { value, span } => {
                        annotate_expr(value, source, file);
                        annotate_span(span, source, file)
                    }
                }
            }
            annotate_span(span, source, file)
        }
    }
}
fn annotate_statement(statement: &mut Statement, source: &str, file: &str) {
    match statement {
        Statement::Let {
            pattern,
            annotation,
            value,
            span,
        } => {
            annotate_pattern(pattern, source, file);
            if let Some(annotation) = annotation {
                annotate_type(annotation, source, file)
            };
            annotate_expr(value, source, file);
            annotate_span(span, source, file)
        }
        Statement::Assignment {
            target,
            value,
            span,
            ..
        } => {
            annotate_assignment_target(target, source, file);
            annotate_expr(value, source, file);
            annotate_span(span, source, file)
        }
        Statement::Assert { value, span }
        | Statement::Expression { value, span }
        | Statement::Control { value, span } => {
            annotate_expr(value, source, file);
            annotate_span(span, source, file)
        }
        Statement::Return { value, span } | Statement::Break { value, span } => {
            if let Some(value) = value {
                annotate_expr(value, source, file)
            };
            annotate_span(span, source, file)
        }
        Statement::Continue { span } => annotate_span(span, source, file),
    }
}
fn annotate_assignment_target(target: &mut AssignmentTarget, source: &str, file: &str) {
    match target {
        AssignmentTarget::Name { span, .. } => annotate_span(span, source, file),
        AssignmentTarget::Field { base, span, .. } => {
            annotate_assignment_target(base, source, file);
            annotate_span(span, source, file)
        }
        AssignmentTarget::Index { base, index, span } => {
            annotate_assignment_target(base, source, file);
            annotate_expr(index, source, file);
            annotate_span(span, source, file)
        }
    }
}
fn annotate_diagnostics(items: &mut [Diagnostic], source: &str, file: &str) {
    for item in items {
        annotate_span(&mut item.span, source, file);
        for label in &mut item.labels {
            annotate_span(&mut label.span, source, file)
        }
        for cause in &mut item.causes {
            annotate_diagnostics(std::slice::from_mut(cause), source, file)
        }
    }
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
    errors: Vec<Diagnostic>,
}
impl Parser {
    fn new(source: &str) -> Self {
        match lex(source) {
            Ok(tokens) => Self {
                tokens,
                at: 0,
                errors: Vec::new(),
            },
            Err(es) => Self {
                tokens: vec![Token {
                    kind: TokenKind::Eof,
                    text: String::new(),
                    span: SourceSpan::new(source.len(), source.len()),
                }],
                at: 0,
                errors: es.into_iter().map(from_lex).collect(),
            },
        }
    }
    /// Cross-production checks whose meaning depends on token order rather
    /// than a declaration-specific AST. This never inspects source text.
    fn validate_token_grammar(&mut self) {
        let tokens = &self.tokens;
        // Reserved legacy forms are recognized as token sequences.  This is
        // deliberately independent of source spelling, comments, file names,
        // and string contents: the lexer has already separated those domains.
        for (i, token) in tokens.iter().enumerate() {
            let TokenKind::Identifier { .. } = token.kind else {
                continue;
            };
            let diagnostic = match token.text.as_str() {
                "var" => Some((
                    "ORNA091-E-VAR",
                    "use `let`; assignment may replace the local slot",
                )),
                "match" => Some(("ORNA091-E-MATCH", "use `case` with colon-delimited arms")),
                "opaque" => Some(("ORNA091-E-OPAQUE", "use the unified `type` declaration")),
                "currency" => Some((
                    "ORNA091-E-CURRENCY",
                    "declare an ordinary nominal type with a nested Currency implementation",
                )),
                "ensure" => Some((
                    "ORNA-A091-010",
                    "`ensure` is not an assertion alias; use `assert`",
                )),
                "fact" => Some((
                    "ORNA-A091-010",
                    "`fact` is not an assertion alias; use `assert`",
                )),
                "ingest"
                    if i == 0
                        || matches!(
                            tokens.get(i.saturating_sub(1)).map(|t| &t.kind),
                            Some(TokenKind::Keyword(Keyword::Pub))
                        ) =>
                {
                    Some(("E1004", "`ingest` is not a declaration"))
                }
                "log"
                    if i == 0
                        || matches!(
                            tokens.get(i.saturating_sub(1)).map(|t| &t.kind),
                            Some(TokenKind::Keyword(Keyword::Pub))
                        ) =>
                {
                    Some(("E1001", "`log` is not a declaration in Orna 1.0"))
                }
                "store"
                    if i == 0
                        || matches!(
                            tokens.get(i.saturating_sub(1)).map(|t| &t.kind),
                            Some(TokenKind::Keyword(Keyword::Pub))
                        ) =>
                {
                    Some(("E1003", "`store` is not a logical declaration"))
                }
                "view"
                    if i == 0
                        || matches!(
                            tokens.get(i.saturating_sub(1)).map(|t| &t.kind),
                            Some(TokenKind::Keyword(Keyword::Pub))
                        ) =>
                {
                    Some(("E1002", "`view` is replaced by an ordinary function"))
                }
                "transaction" if i == 0 => Some((
                    "E1007",
                    "database writes are transactional by activation; `transaction` is not syntax",
                )),
                "on" if i == 0 => Some(("E1005", "`on` is not a declaration")),
                "check" => Some((
                    "ORNA091-E-FIELD-CONSTRAINT",
                    "field `check` syntax was removed; use a table assertion",
                )),
                "unique" => Some((
                    "ORNA091-E-FIELD-CONSTRAINT",
                    "field `unique` syntax was removed; use all_unique in the table body",
                )),
                "where" => Some((
                    "ORNA-A091-001",
                    "use a brace-delimited refined-type assertion block",
                )),
                "constraints" => Some((
                    "ORNA-A091-010",
                    "`constraints` is not a declaration; use owner-local `assert`",
                )),
                _ => None,
            };
            if let Some((code, message)) = diagnostic {
                self.errors
                    .push(Diagnostic::error(code, message, token.span.clone()));
                break;
            }
        }
        for i in 0..tokens.len().saturating_sub(1) {
            let diagnostic = match (&tokens[i].kind, &tokens[i + 1].kind) {
                (TokenKind::Keyword(Keyword::Static), TokenKind::Keyword(Keyword::Fn)) => Some((
                    "ORNA091-E-STATIC-FN",
                    "protocols use instance functions and static properties, not static functions",
                )),
                (_, TokenKind::Punct("?"))
                    if matches!(
                        tokens[i].kind,
                        TokenKind::Integer | TokenKind::Punct(")" | "]")
                    ) =>
                {
                    Some((
                        "ORNA091-E-POSTFIX-QUESTION",
                        if matches!(tokens[i].kind, TokenKind::Integer) {
                            "postfix propagation `?` was removed; failures propagate automatically"
                        } else {
                            "remove postfix `?`; failure propagation is automatic"
                        },
                    ))
                }
                (TokenKind::Punct("|"), TokenKind::Punct("!")) => Some((
                    "ORNA-A091-010",
                    "`|!` is not assertion syntax; use `assert`",
                )),
                _ => None,
            };
            if let Some((code, message)) = diagnostic {
                self.errors
                    .push(Diagnostic::error(code, message, tokens[i].span.clone()));
                break;
            }
        }
        for i in 0..tokens.len().saturating_sub(4) {
            if matches!(tokens[i].kind, TokenKind::Punct("|"))
                && matches!(tokens[i + 1].kind, TokenKind::Identifier { .. })
                && matches!(tokens[i + 2].kind, TokenKind::Punct("("))
                && matches!(tokens[i + 3].kind, TokenKind::Punct("|"))
            {
                self.errors.push(Diagnostic::error(
                    "E1011",
                    "pipe-delimited anonymous functions are not valid; use `=>`",
                    tokens[i + 3].span.clone(),
                ));
                break;
            }
            if matches!(tokens[i].kind, TokenKind::Punct("??"))
                && matches!(tokens[i + 1].kind, TokenKind::Punct("?"))
            {
                self.errors.push(Diagnostic::error(
                    "ORNA091-E-POSTFIX-QUESTION",
                    "standalone postfix `?` has no expression meaning in Orna 1.0",
                    tokens[i + 1].span.clone(),
                ));
                break;
            }
        }
        let mut braces = 0usize;
        for (i, token) in tokens.iter().enumerate() {
            if matches!(token.kind, TokenKind::Punct("{")) {
                braces += 1;
                continue;
            }
            if matches!(token.kind, TokenKind::Punct("}")) {
                braces = braces.saturating_sub(1);
                continue;
            }
            let next = tokens.get(i + 1);
            let next2 = tokens.get(i + 2);
            let diagnostic = if braces == 0
                && matches!(token.kind, TokenKind::Keyword(Keyword::Impl))
                && (i == 0
                    || matches!(
                        tokens.get(i.saturating_sub(1)).map(|t| &t.kind),
                        Some(TokenKind::Keyword(Keyword::Pub))
                    )) {
                Some((
                    "ORNA091-E-IMPL-FOR",
                    "move the implementation inside its nominal target and omit `for Type`",
                ))
            } else if matches!(token.kind, TokenKind::Punct("<"))
                && matches!(next.map(|t| &t.kind), Some(TokenKind::Identifier { .. }))
                && matches!(next2.map(|t| &t.kind), Some(TokenKind::Punct(":")))
            {
                Some((
                    "ORNA091-E-BOUND-COLON",
                    "protocol bounds use `<T impl Protocol>`",
                ))
            } else if matches!(token.kind, TokenKind::Punct("||")) {
                Some(("E1012", "`||` is logical OR, not an anonymous function"))
            } else if matches!(token.kind, TokenKind::Punct("-"))
                && matches!(next.map(|t| &t.kind), Some(TokenKind::Punct(">")))
            {
                Some((
                    "ORNA091-E-RETURN-ARROW",
                    "function return annotations use `: Type`",
                ))
            } else {
                None
            };
            if let Some((code, message)) = diagnostic {
                self.errors
                    .push(Diagnostic::error(code, message, token.span.clone()));
                break;
            }
        }
        for i in 0..tokens.len().saturating_sub(1) {
            if matches!(tokens[i].kind, TokenKind::Keyword(Keyword::Assert))
                && matches!(tokens[i + 1].kind, TokenKind::Punct(";"))
            {
                self.errors.push(Diagnostic::error(
                    "ORNA-A091-011",
                    "assertion requires a proposition before `;`",
                    tokens[i + 1].span.clone(),
                ));
            }
            if matches!(tokens[i].kind, TokenKind::Punct("|"))
                && matches!(
                    tokens.get(i + 1).map(|x| &x.kind),
                    Some(TokenKind::Identifier { .. })
                )
                && matches!(
                    tokens.get(i + 2).map(|x| &x.kind),
                    Some(TokenKind::Punct("=>"))
                )
            {
                self.errors.push(Diagnostic::error(
                    "E1204",
                    "anonymous function must be parenthesized as a pipeline stage",
                    tokens[i + 1].span.clone(),
                ));
            }
        }
        for i in 0..tokens.len().saturating_sub(2) {
            if matches!(tokens[i].kind, TokenKind::Punct("("))
                && matches!(tokens[i + 1].kind, TokenKind::Identifier { .. })
                && matches!(tokens[i + 2].kind, TokenKind::Punct("="))
            {
                let parameter_list =
                    i >= 2 && matches!(tokens[i - 2].kind, TokenKind::Keyword(Keyword::Fn));
                if !parameter_list {
                    self.errors.push(Diagnostic::error(
                        "E1301",
                        "assignment is a statement, not an expression",
                        tokens[i + 2].span.clone(),
                    ));
                }
            }
        }
        // A comparison is deliberately non-associative. The shape below is
        // evaluated only in expression position (after an equals sign), so
        // generic parameter brackets remain type syntax.
        let mut expression_position = false;
        for i in 0..tokens.len().saturating_sub(2) {
            if matches!(tokens[i].kind, TokenKind::Punct("=" | "=>")) {
                expression_position = true;
                continue;
            }
            if matches!(tokens[i].kind, TokenKind::Punct(";" | "{" | "}")) {
                expression_position = false;
                continue;
            }
            if expression_position
                && matches!(
                    tokens[i].kind,
                    TokenKind::Punct("<" | "<=" | ">" | ">=" | "==" | "!=")
                )
                && matches!(
                    tokens[i + 1].kind,
                    TokenKind::Identifier { .. }
                        | TokenKind::Integer
                        | TokenKind::Decimal
                        | TokenKind::Float
                )
                && matches!(
                    tokens[i + 2].kind,
                    TokenKind::Punct("<" | "<=" | ">" | ">=" | "==" | "!=")
                )
            {
                self.errors.push(Diagnostic::error(
                    "E1302",
                    "comparison operators do not chain; write an explicit conjunction",
                    tokens[i + 2].span.clone(),
                ));
                break;
            }
        }
        for i in 0..tokens.len().saturating_sub(1) {
            if !matches!(tokens[i].kind, TokenKind::Keyword(Keyword::Assert)) {
                continue;
            }
            let mut nesting = 0usize;
            let mut terminated = false;
            let mut legacy_else = false;
            for token in &tokens[i + 1..] {
                if matches!(token.kind, TokenKind::Punct("(" | "[" | "{")) {
                    nesting += 1
                }
                if matches!(token.kind, TokenKind::Punct(")" | "]" | "}")) {
                    if nesting == 0 {
                        break;
                    }
                    nesting -= 1
                }
                if nesting == 0 && matches!(token.kind, TokenKind::Punct(";")) {
                    terminated = true;
                    break;
                }
                if nesting == 0 && matches!(token.kind, TokenKind::Keyword(Keyword::Else)) {
                    legacy_else = true
                }
            }
            if !terminated || legacy_else {
                self.errors.push(Diagnostic::error(
                    if legacy_else {
                        "ORNA-A091-006"
                    } else {
                        "ORNA-A091-005"
                    },
                    if legacy_else {
                        "assert has no assertion-specific `else` arm"
                    } else {
                        "assertion clause must end with `;`"
                    },
                    tokens[i].span.clone(),
                ));
            }
        }
    }
    fn item(&mut self, module: bool) -> Option<Item> {
        let start = self.current().span.start;
        let mut visibility = Visibility::Private;
        if self.keyword() == Some(Keyword::Pub) {
            visibility = Visibility::Public {
                span: self.current().span.clone(),
            };
            self.bump()
        }
        let kind = match self.keyword() {
            Some(Keyword::Use) if matches!(&visibility, Visibility::Private) => ItemKind::Use,
            Some(Keyword::Table) => ItemKind::Table,
            Some(Keyword::Fn) => ItemKind::Function,
            Some(Keyword::Protocol) => ItemKind::Protocol,
            Some(Keyword::Enum) => ItemKind::Enum,
            Some(Keyword::Dim) => ItemKind::Dimension,
            Some(Keyword::Unit) => ItemKind::Unit,
            Some(Keyword::Type) => ItemKind::Type,
            Some(Keyword::Assert) if matches!(&visibility, Visibility::Private) => ItemKind::Assert,
            Some(Keyword::Let) if !module && matches!(&visibility, Visibility::Private) => {
                ItemKind::Let
            }
            _ => {
                if module {
                    let code = if matches!(self.current().kind, TokenKind::Identifier { .. })
                        && matches!(
                            self.tokens.get(self.at + 1).map(|t| &t.kind),
                            Some(TokenKind::Punct("(" | "."))
                        ) {
                        "E1006"
                    } else {
                        "ORNA-PARSE-001"
                    };
                    self.error_here(code, "module top level accepts declarations only")
                } else {
                    self.error_here("ORNA-PARSE-001", "expected a REPL declaration")
                };
                return None;
            }
        };
        self.bump();
        let declaration = match kind {
            ItemKind::Use => {
                let (path, tail) = self.parse_use_declaration();
                Declaration::Use { path, tail }
            }
            ItemKind::Function => {
                let name = self.require_name_text();
                let (signature, body) = self.parse_function_tail(name, start);
                Declaration::Function { signature, body }
            }
            ItemKind::Table | ItemKind::Protocol => {
                let name = self.require_name_text();
                let generics = if self.is_punct("<") {
                    self.parse_generic_parameters()
                } else {
                    Vec::new()
                };
                let keys = if self.is_punct("(") {
                    self.parse_function_parameters()
                } else {
                    Vec::new()
                };
                if self.is_punct("{") {
                    if kind == ItemKind::Table {
                        Declaration::Table {
                            name,
                            keys,
                            members: self.parse_table_body(),
                        }
                    } else {
                        Declaration::Protocol {
                            name,
                            generics,
                            members: self.parse_protocol_body(),
                        }
                    }
                } else {
                    self.error_here("ORNA-PARSE-001", "expected declaration body");
                    if kind == ItemKind::Table {
                        Declaration::Table {
                            name,
                            keys,
                            members: Vec::new(),
                        }
                    } else {
                        Declaration::Protocol {
                            name,
                            generics,
                            members: Vec::new(),
                        }
                    }
                }
            }
            ItemKind::Dimension => {
                let name = self.require_name_text();
                let expression = if self.is_punct("=") {
                    self.bump();
                    self.parse_dimension_expression()
                } else {
                    None
                };
                self.require_semicolon("dimension declarations require `;`");
                Declaration::Dimension { name, expression }
            }
            ItemKind::Unit => {
                let name = self.require_name_text();
                self.require_punct(":", "expected `:` after unit name");
                let dimension = self.parse_type_expr().unwrap_or_else(|| self.error_type());
                let definition = if self.keyword() == Some(Keyword::Base) {
                    self.bump();
                    UnitDefinition::Base
                } else if self.is_punct("=") {
                    self.bump();
                    let value = self.expr().unwrap_or_else(|| self.error_expr());
                    let offset = if self.keyword() == Some(Keyword::Offset) {
                        self.bump();
                        self.expr()
                    } else {
                        None
                    };
                    let affine = if self.keyword() == Some(Keyword::Affine) {
                        self.bump();
                        true
                    } else {
                        false
                    };
                    UnitDefinition::Derived {
                        value,
                        offset,
                        affine,
                    }
                } else {
                    self.error_here(
                        "ORNA-PARSE-001",
                        "expected `base` or `=` in unit declaration",
                    );
                    UnitDefinition::Base
                };
                self.require_semicolon("unit declarations require `;`");
                Declaration::Unit {
                    name,
                    dimension,
                    definition,
                }
            }
            ItemKind::Assert => {
                let value = self.parse_assertion_expression();
                self.require_semicolon("assertion statements require `;`");
                Declaration::Assertion { value }
            }
            ItemKind::Let => {
                let pattern = self.parse_pattern();
                let annotation = if self.is_punct(":") {
                    self.bump();
                    self.parse_type_expr()
                } else {
                    None
                };
                self.require_punct("=", "expected `=` in let statement");
                let value = self.expr().unwrap_or_else(|| self.error_expr());
                self.require_semicolon("let statements require `;`");
                Declaration::Let {
                    pattern,
                    annotation,
                    value,
                }
            }
            ItemKind::Enum | ItemKind::Type => {
                let name = self.require_name_text();
                let generics = if self.is_punct("<") {
                    self.parse_generic_parameters()
                } else {
                    Vec::new()
                };
                if kind == ItemKind::Enum && self.is_punct("{") {
                    Declaration::Enum {
                        name,
                        generics,
                        variants: self.parse_enum_body(),
                    }
                } else if kind == ItemKind::Type && self.is_punct("{") {
                    Declaration::Type {
                        name,
                        generics,
                        representation: TypeRepresentation::Nominal {
                            members: self.parse_type_body(),
                        },
                    }
                } else if self.is_punct("=") {
                    self.bump();
                    let ty = self.parse_type_expr().unwrap_or_else(|| self.error_type());
                    if self.is_punct(";") {
                        self.bump();
                        if kind == ItemKind::Type {
                            Declaration::Type {
                                name,
                                generics,
                                representation: TypeRepresentation::Alias {
                                    ty,
                                    refinements: Vec::new(),
                                },
                            }
                        } else {
                            self.error_here("ORNA-PARSE-001", "enums require a nominal body");
                            Declaration::Enum {
                                name,
                                generics,
                                variants: Vec::new(),
                            }
                        }
                    } else if self.is_punct("{") {
                        let refinements = self.parse_type_body();
                        if kind == ItemKind::Type {
                            Declaration::Type {
                                name,
                                generics,
                                representation: TypeRepresentation::Alias { ty, refinements },
                            }
                        } else {
                            self.error_here("ORNA-PARSE-001", "enums require a nominal body");
                            Declaration::Enum {
                                name,
                                generics,
                                variants: Vec::new(),
                            }
                        }
                    } else {
                        self.error_here("ORNA-PARSE-001", "expected `;` or refinement body");
                        if kind == ItemKind::Type {
                            Declaration::Type {
                                name,
                                generics,
                                representation: TypeRepresentation::Alias {
                                    ty,
                                    refinements: Vec::new(),
                                },
                            }
                        } else {
                            Declaration::Enum {
                                name,
                                generics,
                                variants: Vec::new(),
                            }
                        }
                    }
                } else if self.is_punct("{") {
                    if kind == ItemKind::Enum {
                        Declaration::Enum {
                            name,
                            generics,
                            variants: self.parse_enum_body(),
                        }
                    } else {
                        Declaration::Type {
                            name,
                            generics,
                            representation: TypeRepresentation::Nominal {
                                members: self.parse_type_body(),
                            },
                        }
                    }
                } else {
                    self.error_here("ORNA-PARSE-001", "expected declaration body");
                    if kind == ItemKind::Enum {
                        Declaration::Enum {
                            name,
                            generics,
                            variants: Vec::new(),
                        }
                    } else {
                        Declaration::Type {
                            name,
                            generics,
                            representation: TypeRepresentation::Nominal {
                                members: Vec::new(),
                            },
                        }
                    }
                }
            }
            ItemKind::Expression => unreachable!(),
        };
        let end = self.previous().span.end;
        Some(Item {
            kind,
            visibility,
            span: SourceSpan::new(start, end),
            declaration,
        })
    }
    fn parse_use_declaration(&mut self) -> (Vec<ImportSegment>, UseTail) {
        let mut path = Vec::new();
        if !self.contextual() {
            self.error_here("ORNA-PARSE-001", "expected namespace path");
            return (path, UseTail::None);
        }
        path.push(ImportSegment {
            name: self.current().text.clone(),
            span: self.current().span.clone(),
        });
        self.bump();
        while self.is_punct(".")
            && matches!(
                self.tokens.get(self.at + 1).map(|t| &t.kind),
                Some(TokenKind::Identifier { .. })
            )
        {
            self.bump();
            path.push(ImportSegment {
                name: self.current().text.clone(),
                span: self.current().span.clone(),
            });
            self.bump();
        }
        let tail = if self.keyword() == Some(Keyword::As) {
            self.bump();
            if self.contextual() || self.is_punct("_") {
                let alias = ImportSegment {
                    name: self.current().text.clone(),
                    span: self.current().span.clone(),
                };
                self.bump();
                UseTail::Alias {
                    name: alias.name,
                    span: alias.span,
                }
            } else {
                self.error_here("ORNA-PARSE-001", "expected use alias");
                UseTail::None
            }
        } else if self.is_punct(".") {
            self.bump();
            if self.is_punct("*") {
                self.bump();
                UseTail::Glob {
                    span: self.previous().span.clone(),
                }
            } else if self.is_punct("{") {
                self.bump();
                let mut names = Vec::new();
                while !self.eof() && !self.is_punct("}") {
                    if !self.contextual() {
                        self.error_here("ORNA-PARSE-001", "expected imported name");
                        break;
                    }
                    names.push(ImportSegment {
                        name: self.current().text.clone(),
                        span: self.current().span.clone(),
                    });
                    self.bump();
                    if self.is_punct(",") {
                        self.bump()
                    } else {
                        break;
                    }
                }
                self.require_punct("}", "unterminated import list");
                UseTail::Names(names)
            } else {
                self.error_here("ORNA-PARSE-001", "expected `*` or import list");
                UseTail::None
            }
        } else {
            UseTail::None
        };
        self.require_semicolon("use declarations require `;`");
        (path, tail)
    }
    fn parse_function_tail(&mut self, name: String, start: usize) -> (FunctionSignature, Expr) {
        let generics = if self.is_punct("<") {
            self.parse_generic_parameters()
        } else {
            Vec::new()
        };
        if self.is_punct("(") {
            let parameters = self.parse_function_parameters();
            let result = if self.is_punct(":") {
                self.bump();
                self.parse_type_expr()
            } else {
                None
            };
            let signature = FunctionSignature {
                name,
                generics,
                parameters,
                result,
                span: SourceSpan::new(start, self.previous().span.end),
            };
            if self.is_punct("=") {
                self.bump();
                let body = self.expr().unwrap_or_else(|| self.error_expr());
                self.require_semicolon("expected `;` after function expression");
                (signature, body)
            } else if self.is_punct("{") {
                (signature, self.parse_block())
            } else {
                self.error_here("ORNA-PARSE-001", "expected `=` or function block");
                (signature, self.error_expr())
            }
        } else {
            self.error_here("ORNA-PARSE-001", "expected function parameter list");
            (
                FunctionSignature {
                    name,
                    generics,
                    parameters: Vec::new(),
                    result: None,
                    span: SourceSpan::new(start, self.current().span.end),
                },
                self.error_expr(),
            )
        }
    }
    fn parse_function_parameters(&mut self) -> Vec<Parameter> {
        let mut parameters = Vec::new();
        if !self.is_punct("(") {
            self.error_here("ORNA-PARSE-001", "expected `(`");
            return parameters;
        }
        self.bump();
        while !self.eof() && !self.is_punct(")") {
            let start = self.current().span.clone();
            let pattern = self.parse_pattern();
            let annotation = if self.is_punct(":") {
                self.bump();
                self.parse_type_expr()
            } else {
                None
            };
            let default = if self.is_punct("=") {
                self.bump();
                self.expr()
            } else {
                None
            };
            let end = default
                .as_ref()
                .map(Expr::span)
                .or_else(|| annotation.as_ref().map(type_span))
                .unwrap_or_else(|| pattern_span(&pattern));
            parameters.push(Parameter {
                pattern,
                annotation,
                default,
                span: start.join(end),
            });
            if self.is_punct(",") {
                self.bump();
                continue;
            }
            if !self.is_punct(")") {
                self.error_here("ORNA-PARSE-001", "expected `,` or `)` after parameter");
                break;
            }
        }
        if self.is_punct(")") {
            self.bump()
        } else {
            self.error_here("ORNA-PARSE-003", "unterminated parameter list")
        }
        parameters
    }
    fn parse_pattern(&mut self) -> Pattern {
        let token = self.current().clone();
        match token.kind {
            TokenKind::Identifier { .. } | TokenKind::Keyword(Keyword::SelfValue) => {
                self.bump();
                if self.is_punct(".") || self.is_punct("{") || self.is_punct("(") {
                    let mut path = vec![NameSegment {
                        text: token.text.clone(),
                        span: token.span.clone(),
                    }];
                    while self.is_punct(".") {
                        self.bump();
                        if self.contextual() {
                            path.push(NameSegment {
                                text: self.current().text.clone(),
                                span: self.current().span.clone(),
                            });
                            self.bump()
                        } else {
                            self.error_here("ORNA-PARSE-001", "expected qualified pattern segment");
                            break;
                        }
                    }
                    let mut arguments = Vec::new();
                    let mut fields = Vec::new();
                    if self.is_punct("{") {
                        self.bump();
                        while !self.eof() && !self.is_punct("}") {
                            if !self.contextual() {
                                self.error_here(
                                    "ORNA-PARSE-001",
                                    "expected enum-record pattern field",
                                );
                                break;
                            }
                            let field = self.current().clone();
                            self.bump();
                            let pattern = if self.is_punct(":") {
                                self.bump();
                                Some(self.parse_pattern())
                            } else {
                                None
                            };
                            let end = pattern
                                .as_ref()
                                .map(pattern_span)
                                .unwrap_or_else(|| field.span.clone());
                            fields.push(PatternField {
                                name: field.text,
                                pattern,
                                span: field.span.join(end),
                            });
                            if self.is_punct(",") {
                                self.bump()
                            } else {
                                break;
                            }
                        }
                        if self.is_punct("}") {
                            self.bump()
                        } else {
                            self.error_here("ORNA-PARSE-003", "unterminated enum-record pattern")
                        }
                    } else if self.is_punct("(") {
                        self.bump();
                        while !self.eof() && !self.is_punct(")") {
                            arguments.push(self.parse_pattern());
                            if self.is_punct(",") {
                                self.bump()
                            } else {
                                break;
                            }
                        }
                        if self.is_punct(")") {
                            self.bump()
                        } else {
                            self.error_here("ORNA-PARSE-003", "unterminated constructor pattern")
                        }
                    }
                    Pattern::Constructor {
                        path,
                        arguments,
                        fields,
                        span: token.span.join(self.previous().span.clone()),
                    }
                } else {
                    Pattern::Name(token.text, token.span)
                }
            }
            TokenKind::Integer
            | TokenKind::Decimal
            | TokenKind::Float
            | TokenKind::Date
            | TokenKind::Instant
            | TokenKind::String
            | TokenKind::Keyword(Keyword::True | Keyword::False | Keyword::Null) => {
                self.bump();
                Pattern::Literal {
                    kind: literal_kind(&token.kind),
                    text: token.text,
                    span: token.span,
                }
            }
            TokenKind::Punct("_") => {
                self.bump();
                Pattern::Wildcard(token.span)
            }
            TokenKind::Punct("(" | "{" | "[") => {
                let open = token.text.clone();
                let close = match open.as_str() {
                    "(" => ")",
                    "{" => "}",
                    _ => "]",
                };
                self.bump();
                if open == "{" {
                    let mut fields = Vec::new();
                    while !self.eof() && !self.is_punct(close) {
                        let field = self.current().clone();
                        if !self.contextual() {
                            self.error_here("ORNA-PARSE-001", "expected record pattern field");
                            break;
                        }
                        self.bump();
                        let value = if self.is_punct(":") {
                            self.bump();
                            Some(self.parse_pattern())
                        } else {
                            None
                        };
                        let span = field.span.clone().join(
                            value
                                .as_ref()
                                .map(pattern_span)
                                .unwrap_or(field.span.clone()),
                        );
                        fields.push((field.text, value, span));
                        if self.is_punct(",") {
                            self.bump()
                        } else {
                            break;
                        }
                    }
                    if self.is_punct(close) {
                        self.bump()
                    } else {
                        self.error_here("ORNA-PARSE-003", "unterminated record pattern")
                    }
                    return Pattern::Record {
                        fields,
                        span: token.span.join(self.previous().span.clone()),
                    };
                }
                let mut elements = Vec::new();
                while !self.eof() && !self.is_punct(close) {
                    elements.push(self.parse_pattern());
                    if self.is_punct(",") {
                        self.bump()
                    } else {
                        break;
                    }
                }
                if self.is_punct(close) {
                    self.bump()
                } else {
                    self.error_here("ORNA-PARSE-003", "unterminated pattern")
                }
                let span = token.span.join(self.previous().span.clone());
                match open.as_str() {
                    "(" => Pattern::Tuple { elements, span },
                    "[" => Pattern::List { elements, span },
                    _ => unreachable!(),
                }
            }
            _ => {
                self.error_here("ORNA-PARSE-001", "expected pattern");
                Pattern::Record {
                    fields: Vec::new(),
                    span: token.span,
                }
            }
        }
    }
    fn parse_interpolated_string(&mut self) -> Expr {
        let start = self.current().span.clone();
        self.bump();
        let mut segments = Vec::new();
        while !self.eof() && !matches!(self.current().kind, TokenKind::StringEnd) {
            match self.current().kind {
                TokenKind::StringText => {
                    let token = self.current().clone();
                    self.bump();
                    segments.push(StringSegment::Text {
                        text: token.text,
                        span: token.span,
                    });
                }
                TokenKind::InterpolationStart => {
                    let segment_start = self.current().span.clone();
                    self.bump();
                    let value = self.expr().unwrap_or_else(|| self.error_expr());
                    if matches!(self.current().kind, TokenKind::InterpolationEnd) {
                        let end = self.current().span.clone();
                        self.bump();
                        segments.push(StringSegment::Expression {
                            value,
                            span: segment_start.join(end),
                        });
                    } else {
                        self.error_here("ORNA-PARSE-003", "unterminated string interpolation");
                        break;
                    }
                }
                _ => {
                    self.error_here("ORNA-PARSE-001", "expected string text or interpolation");
                    break;
                }
            }
        }
        if matches!(self.current().kind, TokenKind::StringEnd) {
            self.bump()
        } else {
            self.error_here("ORNA-PARSE-003", "unterminated interpolated string")
        }
        Expr::InterpolatedString {
            segments,
            span: start.join(self.previous().span.clone()),
        }
    }
    #[allow(dead_code)]
    fn consume_statement(&mut self) {
        let start = self.at;
        let mut depth = 0;
        while !self.eof() {
            if self.is_punct(";") && depth == 0 {
                self.bump();
                return;
            }
            if self.is_punct("{") || self.is_punct("(") || self.is_punct("[") {
                depth += 1
            } else if self.is_punct("}") || self.is_punct(")") || self.is_punct("]") {
                if depth == 0 {
                    break;
                }
                depth -= 1
            }
            self.bump()
        }
        let span = self
            .tokens
            .get(start)
            .map(|x| x.span.clone())
            .unwrap_or_else(|| self.current().span.clone());
        self.errors.push(Diagnostic::error(
            "ORNA-PARSE-002",
            "expected `;` before end of statement",
            span,
        ))
    }
    fn parse_block(&mut self) -> Expr {
        let start = self.current().span.clone();
        let mut statements = Vec::new();
        let mut tail = None;
        if !self.is_punct("{") {
            self.error_here("ORNA-PARSE-001", "expected block");
            return Expr::Block {
                statements,
                tail,
                span: start,
            };
        }
        self.bump();
        while !self.eof() && !self.is_punct("}") {
            match self.keyword() {
                Some(Keyword::Let) => {
                    let statement_start = self.current().span.clone();
                    self.bump();
                    let pattern = self.parse_pattern();
                    let annotation = if self.is_punct(":") {
                        self.bump();
                        self.parse_type_expr()
                    } else {
                        None
                    };
                    if self.is_punct("=") {
                        self.bump();
                        let value = self.expr().unwrap_or(Expr::Tuple {
                            elements: Vec::new(),
                            span: statement_start.clone(),
                        });
                        let end = value.span();
                        statements.push(Statement::Let {
                            pattern,
                            annotation,
                            value,
                            span: statement_start.join(end),
                        });
                    } else {
                        self.error_here("ORNA-PARSE-001", "expected `=` in let statement")
                    }
                    if self.is_punct(";") {
                        self.bump()
                    } else {
                        self.error_here("ORNA-PARSE-002", "expected `;` after let statement");
                        break;
                    }
                }
                Some(Keyword::Assert) => {
                    let statement_start = self.current().span.clone();
                    self.bump();
                    if self.is_punct(";") {
                        self.error_here("ORNA-PARSE-001", "assert requires an expression")
                    } else {
                        let value = self.expr().unwrap_or(Expr::Tuple {
                            elements: Vec::new(),
                            span: statement_start.clone(),
                        });
                        let end = value.span();
                        statements.push(Statement::Assert {
                            value,
                            span: statement_start.join(end),
                        });
                    }
                    if self.is_punct(";") {
                        self.bump()
                    } else {
                        self.error_here("ORNA-PARSE-002", "assertion statements require `;`");
                        break;
                    }
                }
                Some(Keyword::Return | Keyword::Break | Keyword::Continue) => {
                    let statement_start = self.current().span.clone();
                    let keyword = self.keyword();
                    self.bump();
                    let value = if !self.is_punct(";") {
                        self.expr()
                    } else {
                        None
                    };
                    let end = value
                        .as_ref()
                        .map(Expr::span)
                        .unwrap_or_else(|| statement_start.clone());
                    match keyword {
                        Some(Keyword::Return) => statements.push(Statement::Return {
                            value,
                            span: statement_start.join(end),
                        }),
                        Some(Keyword::Break) => statements.push(Statement::Break {
                            value,
                            span: statement_start.join(end),
                        }),
                        _ => statements.push(Statement::Continue {
                            span: statement_start,
                        }),
                    }
                    if self.is_punct(";") {
                        self.bump()
                    } else {
                        self.error_here("ORNA-PARSE-002", "expected `;` after control statement");
                        break;
                    }
                }
                Some(
                    Keyword::For | Keyword::While | Keyword::Loop | Keyword::If | Keyword::Case,
                ) => {
                    let value = self.parse_control_expression();
                    let span = value.span();
                    statements.push(Statement::Control { value, span });
                    if self.is_punct(";") {
                        self.bump()
                    }
                }
                _ => {
                    if self.assignment_follows() {
                        let statement_start = self.current().span.clone();
                        let target = self.parse_assignment_target();
                        let operator = match self.current().text.as_str() {
                            "=" => AssignmentOperator::Set,
                            "+=" => AssignmentOperator::Add,
                            "-=" => AssignmentOperator::Subtract,
                            "*=" => AssignmentOperator::Multiply,
                            "/=" => AssignmentOperator::Divide,
                            _ => unreachable!(
                                "assignment_follows accepted a non-assignment operator"
                            ),
                        };
                        self.bump();
                        let value = self.expr().unwrap_or(Expr::Tuple {
                            elements: Vec::new(),
                            span: statement_start.clone(),
                        });
                        let end = value.span();
                        statements.push(Statement::Assignment {
                            target,
                            operator,
                            value,
                            span: statement_start.join(end),
                        });
                    } else {
                        let value = match self.expr() {
                            Some(value) => value,
                            None => break,
                        };
                        let span = value.span();
                        if self.is_punct(";") {
                            statements.push(Statement::Expression { value, span })
                        } else {
                            tail = Some(Box::new(value));
                            break;
                        }
                    }
                    if self.is_punct(";") {
                        self.bump()
                    } else {
                        break;
                    }
                }
            }
        }
        if self.is_punct("}") {
            self.bump()
        } else {
            self.error_here("ORNA-PARSE-003", "unterminated block")
        }
        Expr::Block {
            statements,
            tail,
            span: start.join(self.previous().span.clone()),
        }
    }
    fn parse_table_body(&mut self) -> Vec<TableMember> {
        let mut members = Vec::new();
        self.bump();
        while !self.eof() && !self.is_punct("}") {
            if self.keyword() == Some(Keyword::Assert) {
                let start = self.current().span.clone();
                self.bump();
                if matches!(
                    self.current().kind,
                    TokenKind::Punct("<" | "<=" | ">" | ">=" | "==" | "!=")
                        | TokenKind::Keyword(Keyword::In)
                ) {
                    self.bump();
                }
                let value = self.parse_assertion_expression();
                if self.is_punct(";") {
                    self.bump()
                } else {
                    self.error_here("ORNA-PARSE-002", "expected `;` after table assertion")
                };
                members.push(TableMember::Assertion {
                    value,
                    span: start.join(self.previous().span.clone()),
                });
                continue;
            }
            if self.keyword() == Some(Keyword::Impl) {
                let start = self.current().span.clone();
                self.bump();
                let implementation = self.parse_implementation(start.clone());
                members.push(TableMember::Implementation {
                    implementation,
                    span: start.join(self.previous().span.clone()),
                });
                continue;
            }
            let start = self.current().span.clone();
            let name = self.current().text.clone();
            self.require_name();
            if !self.is_punct(":") {
                self.error_here("ORNA-PARSE-001", "expected `:` after table field name");
                break;
            }
            self.bump();
            let ty = self.parse_type_expr().unwrap_or_else(|| self.error_type());
            let initializer = if self.is_punct("=") {
                self.bump();
                Some(FieldInitializer::Default(
                    self.expr().unwrap_or_else(|| self.error_expr()),
                ))
            } else if self.is_punct("=>") {
                self.bump();
                Some(FieldInitializer::Computed(
                    self.expr().unwrap_or_else(|| self.error_expr()),
                ))
            } else {
                None
            };
            if self.is_punct(",") {
                self.bump();
                members.push(TableMember::Field {
                    name,
                    ty,
                    initializer,
                    span: start.join(self.previous().span.clone()),
                });
            } else {
                self.error_here("ORNA-PARSE-001", "table fields require trailing `,`");
                break;
            }
        }
        if self.is_punct("}") {
            self.bump()
        } else {
            self.error_here("ORNA-PARSE-003", "unterminated table body")
        }
        members
    }
    fn parse_protocol_body(&mut self) -> Vec<ProtocolMember> {
        let mut members = Vec::new();
        self.bump();
        while !self.eof() && !self.is_punct("}") {
            if self.keyword() == Some(Keyword::Static) {
                let start = self.current().span.clone();
                self.bump();
                if self.keyword() == Some(Keyword::Fn) {
                    self.error_here(
                        "ORNA-PARSE-001",
                        "protocol static properties are not functions",
                    );
                    break;
                }
                let name = self.current().text.clone();
                self.require_name();
                let ty = if self.is_punct(":") {
                    self.bump();
                    self.parse_type_expr().unwrap_or_else(|| self.error_type())
                } else {
                    self.error_here("ORNA-PARSE-001", "expected `:` after static property");
                    self.error_type()
                };
                if self.is_punct(";") {
                    self.bump();
                    members.push(ProtocolMember::Static {
                        name,
                        ty,
                        span: start.join(self.previous().span.clone()),
                    })
                } else {
                    self.error_here("ORNA-PARSE-002", "expected `;` after static property")
                }
            } else if self.keyword() == Some(Keyword::Fn) {
                let start = self.current().span.clone();
                self.bump();
                let name = self.require_name_text();
                let generics = if self.is_punct("<") {
                    self.parse_generic_parameters()
                } else {
                    Vec::new()
                };
                let parameters = self.parse_function_parameters();
                let result = if self.is_punct(":") {
                    self.bump();
                    self.parse_type_expr()
                } else {
                    None
                };
                let signature = FunctionSignature {
                    name,
                    generics,
                    parameters,
                    result,
                    span: start.clone().join(self.previous().span.clone()),
                };
                if self.is_punct(";") {
                    self.bump();
                    members.push(ProtocolMember::Function {
                        signature,
                        span: start.join(self.previous().span.clone()),
                    })
                } else {
                    self.error_here("ORNA-PARSE-002", "protocol functions require `;`")
                }
            } else {
                self.error_here("ORNA-PARSE-001", "expected protocol member");
                break;
            }
        }
        if self.is_punct("}") {
            self.bump()
        } else {
            self.error_here("ORNA-PARSE-003", "unterminated protocol body")
        }
        members
    }
    fn parse_enum_body(&mut self) -> Vec<EnumVariant> {
        let mut variants = Vec::new();
        self.bump();
        while !self.eof() && !self.is_punct("}") {
            let start = self.current().span.clone();
            if !self.contextual() {
                self.error_here("ORNA-PARSE-001", "expected enum variant");
                break;
            }
            let name = self.current().text.clone();
            self.bump();
            let mut fields = Vec::new();
            if self.is_punct("{") {
                self.bump();
                while !self.eof() && !self.is_punct("}") {
                    if !self.contextual() {
                        self.error_here("ORNA-PARSE-001", "expected enum payload field");
                        break;
                    }
                    let field_start = self.current().span.clone();
                    let field_name = self.current().text.clone();
                    self.bump();
                    if self.is_punct(":") {
                        self.bump();
                        let ty = self.parse_type_expr().unwrap_or_else(|| self.error_type());
                        fields.push(EnumPayloadField {
                            name: field_name,
                            ty,
                            span: field_start.join(self.previous().span.clone()),
                        });
                    } else {
                        self.error_here("ORNA-PARSE-001", "expected `:` after enum payload field");
                        break;
                    }
                    if self.is_punct(",") {
                        self.bump()
                    } else {
                        break;
                    }
                }
                if self.is_punct("}") {
                    self.bump()
                } else {
                    self.error_here("ORNA-PARSE-003", "unterminated enum payload")
                }
            }
            variants.push(EnumVariant {
                name,
                fields,
                span: start.join(self.previous().span.clone()),
            });
            if self.is_punct(",") {
                self.bump()
            } else {
                break;
            }
        }
        if self.is_punct("}") {
            self.bump()
        } else {
            self.error_here("ORNA-PARSE-003", "unterminated enum body")
        }
        variants
    }
    fn parse_type_body(&mut self) -> Vec<TypeMember> {
        let mut members = Vec::new();
        self.bump();
        while !self.eof() && !self.is_punct("}") {
            let start = self.current().span.clone();
            if self.keyword() == Some(Keyword::Assert) {
                self.bump();
                let value = self.parse_assertion_expression();
                if self.is_punct(";") {
                    self.bump()
                } else {
                    self.error_here("ORNA-PARSE-002", "expected `;` after type assertion")
                }
                members.push(TypeMember::Assertion {
                    value,
                    span: start.join(self.previous().span.clone()),
                });
            } else if self.keyword() == Some(Keyword::Impl) {
                self.bump();
                let implementation = self.parse_implementation(start.clone());
                members.push(TypeMember::Implementation {
                    implementation,
                    span: start.join(self.previous().span.clone()),
                });
            } else {
                let visibility = if self.keyword() == Some(Keyword::Pub) {
                    self.bump();
                    true
                } else {
                    false
                };
                let name = self.current().text.clone();
                self.require_name();
                let ty = if self.is_punct(":") {
                    self.bump();
                    self.parse_type_expr().unwrap_or_else(|| self.error_type())
                } else {
                    self.error_here("ORNA-PARSE-001", "expected `:` after type field");
                    self.error_type()
                };
                let initializer = if self.is_punct("=") {
                    self.bump();
                    self.expr()
                } else {
                    None
                };
                if self.is_punct(",") {
                    self.bump()
                } else {
                    self.error_here("ORNA-PARSE-001", "type fields require trailing `,`");
                    break;
                }
                members.push(TypeMember::Field {
                    visibility,
                    name,
                    ty,
                    initializer,
                    span: start.join(self.previous().span.clone()),
                });
            }
        }
        if self.is_punct("}") {
            self.bump()
        } else {
            self.error_here("ORNA-PARSE-003", "unterminated type body")
        }
        members
    }
    fn parse_implementation(&mut self, start: SourceSpan) -> Implementation {
        let protocol = self.parse_type_expr().unwrap_or_else(|| self.error_type());
        self.require_punct("{", "expected implementation body");
        let mut members = Vec::new();
        while !self.eof() && !self.is_punct("}") {
            let member_start = self.current().span.clone();
            if self.keyword() == Some(Keyword::Fn) {
                self.bump();
                let name = self.require_name_text();
                let (signature, body) = self.parse_function_tail(name, member_start.start);
                let span = member_start.join(body.span());
                members.push(ImplMember::Function {
                    signature,
                    body,
                    span,
                });
            } else if self.keyword() == Some(Keyword::Static) {
                self.bump();
                let name = self.require_name_text();
                let ty = if self.is_punct(":") {
                    self.bump();
                    self.parse_type_expr()
                } else {
                    None
                };
                self.require_punct("=", "expected `=` in implementation static property");
                let value = self.expr().unwrap_or_else(|| self.error_expr());
                self.require_semicolon("implementation static properties require `;`");
                let span = member_start.join(self.previous().span.clone());
                members.push(ImplMember::Static {
                    name,
                    ty,
                    value,
                    span,
                });
            } else {
                self.error_here("ORNA-PARSE-001", "expected implementation member");
                break;
            }
        }
        self.require_punct("}", "unterminated implementation body");
        Implementation {
            protocol,
            members,
            span: start.join(self.previous().span.clone()),
        }
    }
    fn parse_generic_parameters(&mut self) -> Vec<GenericParameter> {
        let mut parameters = Vec::new();
        self.require_punct("<", "expected `<`");
        while !self.eof() && !self.is_punct(">") {
            let start = self.current().span.clone();
            let name = self.require_name_text();
            let mut bounds = Vec::new();
            if self.keyword() == Some(Keyword::Impl) {
                self.bump();
                while let Some(bound) = self.parse_type_expr() {
                    bounds.push(bound);
                    if self.is_punct("+") {
                        self.bump()
                    } else {
                        break;
                    }
                }
            }
            let end = bounds
                .last()
                .map(type_span)
                .unwrap_or_else(|| self.previous().span.clone());
            parameters.push(GenericParameter {
                name,
                bounds,
                span: start.join(end),
            });
            if self.is_punct(",") {
                self.bump()
            } else {
                break;
            }
        }
        self.require_punct(">", "unterminated generic parameter list");
        parameters
    }
    fn parse_type_expr(&mut self) -> Option<TypeExpr> {
        let token = self.current().clone();
        let mut ty = match token.kind {
            TokenKind::Keyword(Keyword::Fn) => {
                self.bump();
                self.require_punct("(", "expected `(` after `fn` type");
                let mut parameters = Vec::new();
                while !self.eof() && !self.is_punct(")") {
                    parameters.push(self.parse_type_expr()?);
                    if self.is_punct(",") {
                        self.bump()
                    } else {
                        break;
                    }
                }
                self.require_punct(")", "unterminated function type");
                self.require_punct(":", "function types require a result type");
                let result = self.parse_type_expr()?;
                TypeExpr::Function {
                    parameters,
                    span: token.span.join(type_span(&result)),
                    result: Box::new(result),
                }
            }
            TokenKind::Identifier { .. } | TokenKind::Keyword(_) => {
                let mut path = vec![token.text];
                self.bump();
                while self.is_punct(".") {
                    self.bump();
                    if !self.contextual() {
                        self.error_here("ORNA-PARSE-001", "expected type path segment");
                        break;
                    };
                    path.push(self.current().text.clone());
                    self.bump()
                }
                let mut arguments = Vec::new();
                if self.is_punct("<") {
                    self.bump();
                    while !self.eof() && !self.is_punct(">") {
                        arguments.push(self.parse_type_expr()?);
                        if self.is_punct(",") {
                            self.bump()
                        } else {
                            break;
                        }
                    }
                    if self.is_punct(">") {
                        self.bump()
                    } else {
                        self.error_here("ORNA-PARSE-003", "unterminated type arguments")
                    }
                }
                TypeExpr::Name {
                    path,
                    arguments,
                    span: token.span.join(self.previous().span.clone()),
                }
            }
            TokenKind::Punct("[") => {
                self.bump();
                let inner = self.parse_type_expr()?;
                if self.is_punct("]") {
                    self.bump()
                } else {
                    self.error_here("ORNA-PARSE-001", "expected `]` after list type")
                };
                TypeExpr::List {
                    span: token.span.join(self.previous().span.clone()),
                    inner: Box::new(inner),
                }
            }
            TokenKind::Punct("(") => {
                self.bump();
                let mut elements = Vec::new();
                let mut trailing_comma = false;
                while !self.eof() && !self.is_punct(")") {
                    elements.push(self.parse_type_expr()?);
                    if self.is_punct(",") {
                        self.bump();
                        trailing_comma = true;
                    } else {
                        break;
                    }
                }
                if self.is_punct(")") {
                    self.bump()
                } else {
                    self.error_here("ORNA-PARSE-003", "unterminated tuple type")
                }
                if elements.len() == 1 && !trailing_comma {
                    elements.pop().expect("one element")
                } else {
                    TypeExpr::Tuple {
                        elements,
                        span: token.span.join(self.previous().span.clone()),
                    }
                }
            }
            TokenKind::Punct("{") => {
                self.bump();
                let mut fields = Vec::new();
                while !self.eof() && !self.is_punct("}") {
                    let field = self.current().clone();
                    self.require_name();
                    if self.is_punct(":") {
                        self.bump()
                    } else {
                        self.error_here("ORNA-PARSE-001", "expected `:` in record type")
                    };
                    let value = self.parse_type_expr()?;
                    fields.push((field.text, value, field.span));
                    if self.is_punct(",") {
                        self.bump()
                    } else {
                        break;
                    }
                }
                if self.is_punct("}") {
                    self.bump()
                } else {
                    self.error_here("ORNA-PARSE-003", "unterminated record type")
                }
                TypeExpr::Record {
                    fields,
                    span: token.span.join(self.previous().span.clone()),
                }
            }
            _ => {
                self.error_here("ORNA-PARSE-001", "expected type expression");
                return None;
            }
        };
        while self.is_punct("?") {
            let start = type_span(&ty);
            self.bump();
            ty = TypeExpr::Optional {
                span: start.join(self.previous().span.clone()),
                inner: Box::new(ty),
            }
        }
        while self.is_punct("*") || self.is_punct("/") {
            let op = self.current().text.clone();
            self.bump();
            let rhs = self.parse_type_expr()?;
            let span = type_span(&ty).join(type_span(&rhs));
            ty = TypeExpr::Product {
                lhs: Box::new(ty),
                op,
                rhs: Box::new(rhs),
                span,
            };
        }
        Some(ty)
    }
    fn expr(&mut self) -> Option<Expr> {
        self.pratt(0)
    }
    fn pratt(&mut self, min: u8) -> Option<Expr> {
        let mut lhs = self.prefix()?;
        loop {
            if self.is_punct("[") {
                let start = lhs.span().start;
                self.bump();
                let index = self.expr().unwrap_or(Expr::Tuple {
                    elements: Vec::new(),
                    span: self.current().span.clone(),
                });
                if self.is_punct("]") {
                    self.bump()
                } else {
                    self.error_here("ORNA-PARSE-001", "expected `]` after index")
                }
                lhs = Expr::Index {
                    base: Box::new(lhs),
                    index: Box::new(index),
                    span: SourceSpan::new(start, self.previous().span.end),
                };
                continue;
            }
            if self.is_punct("(") {
                let start = lhs.span().start;
                let arguments = self.parse_arguments();
                lhs = Expr::Call {
                    span: SourceSpan::new(start, self.previous().span.end),
                    callee: Box::new(lhs),
                    arguments,
                };
                continue;
            }
            if self.is_punct(".") {
                let start = lhs.span().start;
                self.bump();
                let t = self.current().clone();
                if self.contextual() {
                    self.bump();
                    lhs = Expr::Field {
                        base: Box::new(lhs),
                        name: t.text,
                        span: SourceSpan::new(start, t.span.end),
                    };
                    continue;
                } else {
                    self.error_here("ORNA-PARSE-001", "expected field name");
                    break;
                }
            }
            let Some((prec, right)) = self.infix() else {
                break;
            };
            if prec < min {
                break;
            }
            let op = self.current().text.clone();
            self.bump();
            if matches!(op.as_str(), "|" | "|?")
                && !matches!(
                    self.current().kind,
                    TokenKind::Identifier { .. }
                        | TokenKind::Keyword(Keyword::SelfValue)
                        | TokenKind::Punct("(")
                )
            {
                self.error_here(
                    "ORNA-PARSE-001",
                    "pipeline stages must be postfix expressions",
                );
                break;
            }
            let rhs = self.pratt(if right { prec } else { prec + 1 });
            let Some(rhs) = rhs else { break };
            let span = lhs.span().join(rhs.span());
            lhs = Expr::Binary {
                lhs: Box::new(lhs),
                op,
                rhs: Box::new(rhs),
                span,
            }
        }
        Some(lhs)
    }
    fn prefix(&mut self) -> Option<Expr> {
        let t = self.current().clone();
        if matches!(
            t.kind,
            TokenKind::Punct("!") | TokenKind::Punct("-") | TokenKind::Punct("+")
        ) {
            self.bump();
            let rhs = self.prefix()?;
            let end = rhs.span();
            return Some(Expr::Unary {
                op: t.text,
                rhs: Box::new(rhs),
                span: t.span.join(end),
            });
        }
        match t.kind {
            TokenKind::Identifier { .. } | TokenKind::Keyword(Keyword::SelfValue) => {
                self.bump();
                if self.is_punct("=>") {
                    self.bump();
                    let body = self.parse_lambda_body();
                    return Some(Expr::Lambda {
                        parameters: vec![LambdaParameter {
                            pattern: Pattern::Name(t.text, t.span.clone()),
                            annotation: None,
                            span: t.span.clone(),
                        }],
                        span: t.span.join(body.span()),
                        body: Box::new(body),
                    });
                }
                if self.is_punct("{") && self.is_direct_record_body() {
                    let fields = self.parse_record();
                    return Some(Expr::Nominal {
                        path: vec![NameSegment {
                            text: t.text,
                            span: t.span.clone(),
                        }],
                        fields,
                        span: t.span.join(self.previous().span.clone()),
                    });
                }
                Some(Expr::Name {
                    text: t.text,
                    span: t.span,
                })
            }
            TokenKind::Integer
            | TokenKind::Decimal
            | TokenKind::Float
            | TokenKind::Date
            | TokenKind::Instant
            | TokenKind::String
            | TokenKind::Keyword(Keyword::True | Keyword::False | Keyword::Null) => {
                self.bump();
                Some(Expr::Literal {
                    text: t.text,
                    kind: literal_kind(&t.kind),
                    span: t.span,
                })
            }
            TokenKind::StringStart => Some(self.parse_interpolated_string()),
            TokenKind::ReplBinding => {
                self.bump();
                Some(Expr::ReplBinding {
                    text: t.text,
                    span: t.span,
                })
            }
            TokenKind::Keyword(
                Keyword::If | Keyword::Case | Keyword::For | Keyword::While | Keyword::Loop,
            ) => Some(self.parse_control_expression()),
            TokenKind::Punct("(") => {
                if self.group_followed_by_arrow() {
                    let parameters = self.parse_lambda_parameters();
                    self.bump();
                    let body = self.parse_lambda_body();
                    return Some(Expr::Lambda {
                        parameters,
                        body: Box::new(body),
                        span: t.span.join(self.previous().span.clone()),
                    });
                }
                self.bump();
                if self.is_punct(")") {
                    self.bump();
                    return Some(Expr::Tuple {
                        elements: Vec::new(),
                        span: t.span.join(self.previous().span.clone()),
                    });
                }
                let inner = self.expr()?;
                if self.is_punct(",") {
                    let mut elements = vec![inner];
                    while self.is_punct(",") {
                        self.bump();
                        if self.is_punct(")") {
                            break;
                        }
                        if let Some(value) = self.expr() {
                            elements.push(value)
                        } else {
                            break;
                        }
                    }
                    if self.is_punct(")") {
                        self.bump()
                    } else {
                        self.error_here("ORNA-PARSE-001", "expected `)` after tuple")
                    }
                    return Some(Expr::Tuple {
                        elements,
                        span: t.span.join(self.previous().span.clone()),
                    });
                }
                if !self.is_punct(")") {
                    self.error_here("ORNA-PARSE-001", "expected `)`")
                } else {
                    self.bump()
                }
                let span = t.span.join(self.previous().span.clone());
                Some(Expr::Group {
                    inner: Box::new(inner),
                    span,
                })
            }
            TokenKind::Punct("{") => {
                let fields = self.parse_record();
                Some(Expr::Record {
                    fields,
                    span: t.span.join(self.previous().span.clone()),
                })
            }
            TokenKind::Punct("[") => {
                let elements = self.parse_list();
                Some(Expr::List {
                    elements,
                    span: t.span.join(self.previous().span.clone()),
                })
            }
            _ => {
                self.error_here("ORNA-PARSE-001", "expected expression");
                None
            }
        }
    }
    fn parse_record(&mut self) -> Vec<RecordField> {
        let mut fields = Vec::new();
        self.bump();
        if self.is_punct("}") {
            self.bump();
            return fields;
        }
        loop {
            if !self.contextual() {
                self.error_here("ORNA-PARSE-001", "expected record field name");
                break;
            }
            let name = self.current().text.clone();
            let start = self.current().span.clone();
            self.bump();
            if !self.is_punct(":") {
                self.error_here("E1013", "record fields require `name: expression`");
                break;
            }
            self.bump();
            let value = self.expr().unwrap_or(Expr::Tuple {
                elements: Vec::new(),
                span: start.clone(),
            });
            let span = start.clone().join(value.span());
            fields.push(RecordField { name, value, span });
            if self.is_punct(",") {
                self.bump();
                if self.is_punct("}") { break } else { continue }
            }
            break;
        }
        if self.is_punct("}") {
            self.bump()
        } else {
            self.error_here("ORNA-PARSE-001", "expected `}` after record")
        }
        fields
    }
    fn parse_arguments(&mut self) -> Vec<Argument> {
        let mut arguments = Vec::new();
        self.bump();
        if self.is_punct(")") {
            self.bump();
            return arguments;
        }
        loop {
            let start = self.current().span.clone();
            let mut name = None;
            if self.contextual()
                && matches!(
                    self.tokens.get(self.at + 1).map(|t| &t.kind),
                    Some(TokenKind::Punct(":"))
                )
            {
                name = Some(self.current().text.clone());
                self.bump();
                self.bump()
            }
            let value = self.expr().unwrap_or(Expr::Tuple {
                elements: Vec::new(),
                span: start.clone(),
            });
            let span = start.join(value.span());
            arguments.push(Argument { name, value, span });
            if self.is_punct(",") {
                self.bump();
                if self.is_punct(")") { break } else { continue }
            }
            break;
        }
        if self.is_punct(")") {
            self.bump()
        } else {
            self.error_here("ORNA-PARSE-001", "expected `)` after arguments")
        }
        arguments
    }
    fn parse_control_expression(&mut self) -> Expr {
        let start = self.current().span.clone();
        let kind = self.keyword();
        self.bump();
        let mut condition = None;
        let mut binding = None;
        let mut body = None;
        let mut arms = Vec::new();
        let mut alternate = None;
        match kind {
            Some(Keyword::Loop) => {
                body = self
                    .parse_required_block("expected loop block")
                    .map(Box::new)
            }
            Some(Keyword::If) => {
                condition = self.expr().map(Box::new);
                body = self.parse_required_block("expected if block").map(Box::new);
                if self.keyword() == Some(Keyword::Else) {
                    self.bump();
                    alternate = if self.keyword() == Some(Keyword::If) {
                        Some(Box::new(self.parse_control_expression()))
                    } else {
                        self.parse_required_block("expected else block")
                            .map(Box::new)
                    };
                }
            }
            Some(Keyword::While) => {
                condition = self.expr().map(Box::new);
                body = self
                    .parse_required_block("expected while block")
                    .map(Box::new);
            }
            Some(Keyword::For) => {
                let pattern = self.parse_pattern();
                binding = Some(pattern.clone());
                if self.keyword() != Some(Keyword::In) {
                    self.error_here("ORNA-PARSE-001", "expected `in` after for pattern")
                } else {
                    self.bump()
                }
                condition = self.expr().map(Box::new);
                body = self
                    .parse_required_block("expected for block")
                    .map(Box::new);
                if let Some(loop_body) = body.as_deref().cloned() {
                    arms.push(CaseArm {
                        pattern,
                        guard: None,
                        body: loop_body,
                        span: start.clone(),
                    });
                }
            }
            Some(Keyword::Case) => {
                condition = self.expr().map(Box::new);
                if !self.is_punct("{") {
                    self.error_here("ORNA-PARSE-001", "expected case arms")
                } else {
                    self.bump();
                    while !self.eof() && !self.is_punct("}") {
                        let arm_start = self.current().span.clone();
                        let pattern = self.parse_pattern();
                        let guard = if self.keyword() == Some(Keyword::If) {
                            self.bump();
                            self.expr()
                        } else {
                            None
                        };
                        if !self.is_punct(":") {
                            self.error_here("ORNA-PARSE-001", "expected `:` after case pattern");
                            break;
                        }
                        self.bump();
                        let arm_body = self.parse_lambda_body();
                        let span = arm_start.join(arm_body.span());
                        arms.push(CaseArm {
                            pattern,
                            guard,
                            body: arm_body,
                            span,
                        });
                        if self.is_punct(",") {
                            self.bump()
                        } else {
                            break;
                        }
                    }
                    if self.is_punct("}") {
                        self.bump()
                    } else {
                        self.error_here("ORNA-PARSE-003", "unterminated case arms")
                    }
                }
            }
            _ => {}
        }
        let control_kind = match kind {
            Some(Keyword::If) => ControlKind::If,
            Some(Keyword::Case) => ControlKind::Case,
            Some(Keyword::For) => ControlKind::For,
            Some(Keyword::While) => ControlKind::While,
            _ => ControlKind::Loop,
        };
        Expr::Control {
            kind: control_kind,
            binding,
            condition,
            body,
            arms,
            alternate,
            span: start.join(self.previous().span.clone()),
        }
    }
    fn parse_required_block(&mut self, message: &str) -> Option<Expr> {
        if self.is_punct("{") {
            Some(self.parse_block())
        } else {
            self.error_here("ORNA-PARSE-001", message);
            None
        }
    }
    fn group_followed_by_arrow(&self) -> bool {
        let mut depth = 0usize;
        for i in self.at..self.tokens.len() {
            match self.tokens[i].kind {
                TokenKind::Punct("(") => depth += 1,
                TokenKind::Punct(")") => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(
                            self.tokens.get(i + 1).map(|t| &t.kind),
                            Some(TokenKind::Punct("=>"))
                        );
                    }
                }
                _ => {}
            }
        }
        false
    }
    fn parse_lambda_parameters(&mut self) -> Vec<LambdaParameter> {
        let mut parameters = Vec::new();
        self.bump();
        while !self.eof() && !self.is_punct(")") {
            let pattern = self.parse_pattern();
            let span = pattern_span(&pattern);
            let annotation = if self.is_punct(":") {
                self.bump();
                self.parse_type_expr()
            } else {
                None
            };
            parameters.push(LambdaParameter {
                pattern,
                annotation,
                span,
            });
            if self.is_punct(",") {
                self.bump()
            } else {
                break;
            }
        }
        if self.is_punct(")") {
            self.bump()
        } else {
            self.error_here("ORNA-PARSE-003", "unterminated lambda parameters")
        }
        parameters
    }
    fn parse_lambda_body(&mut self) -> Expr {
        if self.is_punct("{") {
            if self.is_direct_record_body() {
                let start = self.current().span.clone();
                let fields = self.parse_record();
                Expr::Record {
                    fields,
                    span: start.join(self.previous().span.clone()),
                }
            } else {
                self.parse_block()
            }
        } else {
            self.expr().unwrap_or(Expr::Tuple {
                elements: Vec::new(),
                span: self.current().span.clone(),
            })
        }
    }
    fn is_direct_record_body(&self) -> bool {
        self.is_punct("{")
            && matches!(
                self.tokens.get(self.at + 1).map(|t| &t.kind),
                Some(TokenKind::Identifier { .. } | TokenKind::Keyword(_))
            )
            && matches!(
                self.tokens.get(self.at + 2).map(|t| &t.kind),
                Some(TokenKind::Punct(":"))
            )
    }
    fn parse_list(&mut self) -> Vec<Expr> {
        let mut elements = Vec::new();
        self.bump();
        if self.is_punct("]") {
            self.bump();
            return elements;
        }
        loop {
            if let Some(element) = self.expr() {
                elements.push(element)
            };
            if self.is_punct(",") {
                self.bump();
                if self.is_punct("]") { break } else { continue }
            }
            break;
        }
        if self.is_punct("]") {
            self.bump()
        } else {
            self.error_here("ORNA-PARSE-001", "expected `]` after list")
        }
        elements
    }
    fn assignment_follows(&self) -> bool {
        if !matches!(self.current().kind, TokenKind::Identifier { .. }) {
            return false;
        }
        let mut at = self.at + 1;
        loop {
            match self.tokens.get(at).map(|token| &token.kind) {
                Some(TokenKind::Punct("."))
                    if matches!(
                        self.tokens.get(at + 1).map(|token| &token.kind),
                        Some(TokenKind::Identifier { .. } | TokenKind::Keyword(_))
                    ) =>
                {
                    at += 2
                }
                Some(TokenKind::Punct("[")) => {
                    let mut depth = 1usize;
                    at += 1;
                    while depth > 0 {
                        match self.tokens.get(at).map(|token| &token.kind) {
                            Some(TokenKind::Punct("[")) => depth += 1,
                            Some(TokenKind::Punct("]")) => depth -= 1,
                            Some(_) => {}
                            None => return false,
                        }
                        at += 1;
                    }
                }
                Some(TokenKind::Punct("=" | "+=" | "-=" | "*=" | "/=")) => return true,
                _ => return false,
            }
        }
    }
    fn parse_assignment_target(&mut self) -> AssignmentTarget {
        let first = self.current().clone();
        self.require_name();
        let mut target = AssignmentTarget::Name {
            name: first.text,
            span: first.span,
        };
        loop {
            if self.is_punct(".") {
                self.bump();
                let field = self.current().clone();
                if self.contextual() {
                    self.bump();
                    let span = assignment_target_span(&target).join(field.span);
                    target = AssignmentTarget::Field {
                        base: Box::new(target),
                        name: field.text,
                        span,
                    };
                } else {
                    self.error_here("ORNA-PARSE-001", "expected assignment field name");
                    break;
                }
            } else if self.is_punct("[") {
                let start = assignment_target_span(&target);
                self.bump();
                let index = self.expr().unwrap_or_else(|| self.error_expr());
                self.require_punct("]", "expected `]` after assignment index");
                target = AssignmentTarget::Index {
                    base: Box::new(target),
                    index,
                    span: start.join(self.previous().span.clone()),
                };
            } else {
                break;
            }
        }
        target
    }
    fn infix(&self) -> Option<(u8, bool)> {
        let s = self.current().text.as_str();
        Some(match s {
            "||" => (1, false),
            "&&" => (2, false),
            "==" | "!=" | "<" | "<=" | ">" | ">=" | "in" => (3, false),
            "??" => (4, true),
            "|" | "|?" => (5, false),
            ".." | "..=" => (6, false),
            "+" | "-" => (7, false),
            "*" | "/" | "%" => (8, false),
            "^" => (9, true),
            _ => return None,
        })
    }
    fn finish(&mut self) {
        if !self.eof() {
            self.error_here("ORNA-PARSE-001", "unexpected trailing input")
        }
    }
    fn require_name(&mut self) {
        if matches!(self.current().kind, TokenKind::Identifier { .. }) {
            self.bump()
        } else {
            self.error_here("ORNA-PARSE-001", "expected identifier")
        }
    }
    fn require_name_text(&mut self) -> String {
        let text = self.current().text.clone();
        self.require_name();
        text
    }
    fn require_punct(&mut self, punct: &str, message: &str) {
        if self.is_punct(punct) {
            self.bump()
        } else {
            self.error_here("ORNA-PARSE-001", message)
        }
    }
    fn require_semicolon(&mut self, message: &str) {
        if self.is_punct(";") {
            self.bump()
        } else {
            self.error_here("ORNA-PARSE-002", message)
        }
    }
    fn error_expr(&self) -> Expr {
        Expr::Tuple {
            elements: Vec::new(),
            span: self.current().span.clone(),
        }
    }
    fn error_type(&self) -> TypeExpr {
        TypeExpr::Tuple {
            elements: Vec::new(),
            span: self.current().span.clone(),
        }
    }
    fn parse_assertion_expression(&mut self) -> Expr {
        if matches!(
            self.current().kind,
            TokenKind::Punct("<" | "<=" | ">" | ">=" | "==" | "!=")
                | TokenKind::Keyword(Keyword::In)
        ) {
            let op = self.current().clone();
            self.bump();
            let rhs = self.expr().unwrap_or_else(|| self.error_expr());
            Expr::Unary {
                op: op.text,
                span: op.span.join(rhs.span()),
                rhs: Box::new(rhs),
            }
        } else {
            self.expr().unwrap_or_else(|| self.error_expr())
        }
    }
    fn parse_dimension_expression(&mut self) -> Option<DimensionExpr> {
        let start = self.current().span.clone();
        let mut terms = Vec::new();
        let mut operators = Vec::new();
        loop {
            let term_start = self.current().span.clone();
            let ty = self.parse_type_expr()?;
            let exponent = if self.is_punct("^") {
                self.bump();
                let sign = if self.is_punct("+") || self.is_punct("-") {
                    let sign = self.current().text.clone();
                    self.bump();
                    sign
                } else {
                    String::new()
                };
                let value = self.current().text.clone();
                if matches!(self.current().kind, TokenKind::Integer) {
                    self.bump()
                } else {
                    self.error_here("ORNA-PARSE-001", "expected signed integer exponent")
                };
                Some(format!("{sign}{value}"))
            } else {
                None
            };
            let end = self.previous().span.clone();
            terms.push((String::new(), ty, exponent, term_start.join(end)));
            if self.is_punct("*") || self.is_punct("/") {
                operators.push(self.current().text.clone());
                self.bump()
            } else {
                break;
            }
        }
        Some(DimensionExpr {
            terms,
            operators,
            span: start.join(self.previous().span.clone()),
        })
    }
    fn recover_top(&mut self) {
        while !self.eof() && !self.is_punct(";") && !self.is_punct("}") {
            self.bump()
        }
        if self.is_punct(";") || self.is_punct("}") {
            self.bump()
        }
    }
    fn contextual(&self) -> bool {
        matches!(
            self.current().kind,
            TokenKind::Identifier { .. } | TokenKind::Keyword(_)
        )
    }
    fn keyword(&self) -> Option<Keyword> {
        if let TokenKind::Keyword(k) = self.current().kind {
            Some(k)
        } else {
            None
        }
    }
    fn is_punct(&self, s: &str) -> bool {
        matches!(&self.current().kind,TokenKind::Punct(p) if *p==s)
    }
    fn eof(&self) -> bool {
        matches!(self.current().kind, TokenKind::Eof)
    }
    fn current(&self) -> &Token {
        &self.tokens[self.at]
    }
    fn previous(&self) -> &Token {
        &self.tokens[self.at.saturating_sub(1)]
    }
    fn bump(&mut self) {
        if !self.eof() {
            self.at += 1
        }
    }
    fn error_here(&mut self, code: &'static str, message: &str) {
        self.errors.push(Diagnostic::error(
            code,
            message,
            self.current().span.clone(),
        ))
    }
}
fn from_lex(e: LexError) -> Diagnostic {
    Diagnostic::error(e.code, e.message, e.span)
}
fn literal_kind(token: &TokenKind) -> LiteralKind {
    match token {
        TokenKind::Integer => LiteralKind::Integer,
        TokenKind::Decimal => LiteralKind::Decimal,
        TokenKind::Float => LiteralKind::Float,
        TokenKind::Date => LiteralKind::Date,
        TokenKind::Instant => LiteralKind::Instant,
        TokenKind::String => LiteralKind::String,
        TokenKind::Keyword(Keyword::True | Keyword::False) => LiteralKind::Boolean,
        TokenKind::Keyword(Keyword::Null) => LiteralKind::Null,
        _ => unreachable!("literal_kind called for a non-literal token"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_resolver_relevant_binding_assignment_and_import_structure() {
        let source = "use std.math.{abs, min,};\npub fn apply() { let { value: item }: { value: Int } = input; account.entries[item].amount += 1; }";
        let parsed = parse_module_with_file(source, "memory.orna");
        assert!(parsed.is_ok(), "{:?}", parsed.diagnostics);
        let Item {
            visibility: Visibility::Private,
            declaration: Declaration::Use { path, tail },
            ..
        } = &parsed.value.items[0]
        else {
            panic!("missing structured import")
        };
        assert_eq!(
            path.iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>(),
            ["std", "math"]
        );
        assert!(
            path.iter()
                .all(|segment| segment.span.file.as_deref() == Some("memory.orna"))
        );
        assert!(
            matches!(tail, UseTail::Names(names) if names.len() == 2 && names[0].name == "abs")
        );
        let Item {
            visibility: Visibility::Public { span },
            declaration:
                Declaration::Function {
                    body: Expr::Block { statements, .. },
                    ..
                },
            ..
        } = &parsed.value.items[1]
        else {
            panic!("missing public function")
        };
        assert_eq!(span.file.as_deref(), Some("memory.orna"));
        assert!(matches!(
            &statements[0],
            Statement::Let {
                pattern: Pattern::Record { .. },
                annotation: Some(TypeExpr::Record { .. }),
                ..
            }
        ));
        assert!(
            matches!(&statements[1], Statement::Assignment { target: AssignmentTarget::Field { base, name, .. }, operator: AssignmentOperator::Add, .. } if name == "amount" && matches!(base.as_ref(), AssignmentTarget::Index { base, .. } if matches!(base.as_ref(), AssignmentTarget::Field { .. })))
        );
    }
}
