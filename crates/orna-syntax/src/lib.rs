//! Lossless source parsing for the Orna language.
//!
//! This crate recognises supported declarations and function bodies.
//! All source bytes remain in the CST, including whitespace and comments.

use std::{fmt, ops::Range};

mod highlight;
mod lexer;
mod parser;

pub use highlight::{HighlightKind, HighlightToken, KEYWORDS, SCALAR_TYPES, highlight};

/// A byte range in the input source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSpan {
    /// The first byte in the range, inclusive.
    pub start: usize,
    /// The first byte after the range, exclusive.
    pub end: usize,
}

impl SourceSpan {
    fn from_range(range: Range<usize>) -> Self {
        Self {
            start: range.start,
            end: range.end,
        }
    }
}

/// A source error emitted by the lexer or the parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// A stable category for programmatic handling.
    pub code: &'static str,
    /// A human-readable description of the error.
    pub message: String,
    /// The part of the source that caused the error.
    pub span: SourceSpan,
}

/// One identifier component in a qualified name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamePart {
    /// The exact source spelling, including quotes when present.
    pub text: String,
    /// The byte range of this component.
    pub span: SourceSpan,
}

/// A qualified Orna name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualifiedName {
    /// The name components in source order.
    pub parts: Vec<NamePart>,
    /// The span from the first component through the final component.
    pub span: SourceSpan,
}

/// A parsed `CREATE SCHEMA` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaDeclaration {
    /// The declared schema name.
    pub name: QualifiedName,
    /// The declaration span, including its terminating semicolon.
    pub span: SourceSpan,
}

/// A type written in an object field, record value field, or function shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeSpecification {
    /// A named type, including source spellings such as `TEXT` and `BOOL`.
    Named(QualifiedName),
    /// A standard large-object scalar written as multiple words.
    ///
    /// This is not a qualified name. Its source slice keeps every source byte
    /// between the words, including comments and whitespace.
    StandardLargeObject {
        /// The standard scalar selected by the written phrase.
        kind: StandardLargeObjectKind,
        /// The exact written standard scalar phrase.
        source: SourceSlice,
    },
    /// A typed reference to another written type specification.
    Reference {
        /// The recursively parsed reference target.
        target: Box<TypeSpecification>,
        /// The span from `REF` through the complete target type.
        span: SourceSpan,
    },
    /// An ordered collection type.
    List {
        /// The element type.
        element: Box<TypeSpecification>,
        /// The complete `LIST<...>` span.
        span: SourceSpan,
    },
    /// A logically unique collection type.
    Set {
        /// The element type.
        element: Box<TypeSpecification>,
        /// The complete `SET<...>` span.
        span: SourceSpan,
    },
    /// A key/value collection type.
    Map {
        /// The key type.
        key: Box<TypeSpecification>,
        /// The value type.
        value: Box<TypeSpecification>,
        /// The complete `MAP<...>` span.
        span: SourceSpan,
    },
    /// An optional value type.
    Option {
        /// The optional value type.
        value: Box<TypeSpecification>,
        /// The exact accepted prefix or postfix spelling.
        spelling: OptionTypeSpelling,
        /// The complete option type span.
        span: SourceSpan,
    },
    /// An execution-time stream type.
    Stream {
        /// The streamed element type.
        element: Box<TypeSpecification>,
        /// The complete `STREAM<...>` span.
        span: SourceSpan,
    },
}

impl TypeSpecification {
    /// Return the span of the written type specification.
    pub fn span(&self) -> &SourceSpan {
        match self {
            Self::Named(name) => &name.span,
            Self::StandardLargeObject { source, .. } => &source.span,
            Self::Reference { span, .. }
            | Self::List { span, .. }
            | Self::Set { span, .. }
            | Self::Map { span, .. }
            | Self::Option { span, .. }
            | Self::Stream { span, .. } => span,
        }
    }
}

/// The source spelling used for an optional type constructor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionTypeSpelling {
    /// The `OPTION<T>` prefix form.
    Prefix,
    /// The `T?` postfix form.
    Postfix,
}

/// The standard large-object scalar phrases recognised by Orna syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandardLargeObjectKind {
    /// `CHARACTER LARGE OBJECT`.
    Character,
    /// `BINARY LARGE OBJECT`.
    Binary,
}

/// A source slice retained for an expression that is not parsed in this slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSlice {
    /// The exact source text in the slice.
    pub text: String,
    /// The byte range of the slice.
    pub span: SourceSpan,
}

/// One parsed `SELECT` query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectQuery {
    /// The duplicate policy selected immediately after `SELECT`.
    pub quantifier: SelectQuantifier,
    /// The expressions selected in source order.
    pub projections: Vec<QueryExpression>,
    /// The object source for the query.
    pub source_object: ObjectSource,
    /// The optional predicate after `WHERE`.
    pub predicate: Option<QueryExpression>,
    /// The ordering expressions after `ORDER BY`, in source order.
    pub ordering: Vec<OrderingExpression>,
    /// The span from `SELECT` through the final query token.
    pub span: SourceSpan,
}

/// The closed duplicate policy for one parsed `SELECT` query.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SelectQuantifier {
    /// The implicit duplicate-preserving form with no written keyword.
    All,
    /// The explicit duplicate-eliminating keyword and its exact source slice.
    Distinct {
        /// The exact written `DISTINCT` token.
        source: SourceSlice,
    },
}

/// An object type read by a `SELECT` query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectSource {
    /// The object type named after `FROM`.
    pub object_type: QualifiedName,
    /// The alias that roots field paths and `REF` expressions.
    pub alias: NamePart,
    /// The span from the object type through the alias.
    pub span: SourceSpan,
}

/// One expression supported by the initial relational query slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryExpression {
    /// The identity of one object source alias, written as `REF(alias)`.
    ObjectReference {
        /// The alias supplied to `REF`.
        alias: NamePart,
        /// The span from `REF` through the closing parenthesis.
        span: SourceSpan,
    },
    /// A path from an object source alias through one or more fields.
    FieldPath {
        /// The object source alias at the start of the path.
        root: NamePart,
        /// The fields selected from the object source in source order.
        members: Vec<NamePart>,
        /// The span from the alias through the final field.
        span: SourceSpan,
    },
    /// A boolean literal, retaining its exact source spelling.
    BooleanLiteral {
        /// The boolean value selected by the source text.
        value: bool,
        /// The exact source spelling of the literal.
        source: SourceSlice,
    },
    /// A bare server function parameter read used as a supported selector.
    ParameterRead {
        /// The parameter name as written in the query.
        parameter: NamePart,
    },
    /// Equality between two supported query expressions.
    Equality {
        /// The expression to the left of `=`.
        left: Box<QueryExpression>,
        /// The expression to the right of `=`.
        right: Box<QueryExpression>,
        /// The span from the left expression through the right expression.
        span: SourceSpan,
    },
}

impl QueryExpression {
    /// Return the complete source span for this expression.
    pub fn span(&self) -> &SourceSpan {
        match self {
            Self::ObjectReference { span, .. }
            | Self::FieldPath { span, .. }
            | Self::Equality { span, .. } => span,
            Self::BooleanLiteral { source, .. } => &source.span,
            Self::ParameterRead { parameter } => &parameter.span,
        }
    }
}

/// One expression in an `ORDER BY` clause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderingExpression {
    /// The expression that determines the order.
    pub expression: QueryExpression,
    /// The explicitly written direction, or the language default.
    pub direction: OrderingDirection,
    /// The explicitly written null order, or the language default.
    pub null_order: NullOrdering,
    /// The span from the expression through its optional direction.
    pub span: SourceSpan,
}

/// The direction of an ordering expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderingDirection {
    /// No direction was written. Later semantic planning applies the language default.
    Unspecified,
    /// `ASC` was written.
    Ascending,
    /// `DESC` was written.
    Descending,
}

/// The null ordering of an ordering expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NullOrdering {
    /// No null ordering was written. Later semantic planning applies the language default.
    Unspecified,
}

/// A parameter declared by a server function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerFunctionParameter {
    /// The parameter name as written in source.
    pub name: NamePart,
    /// The zero-based source order of the parameter.
    pub order: usize,
    /// The declared parameter type.
    pub type_specification: TypeSpecification,
    /// The unparsed default expression source, if one was declared.
    pub default_expression: Option<SourceSlice>,
    /// The documentation text declared by a `DOCUMENTATION` modifier.
    pub documentation: Option<SourceSlice>,
    /// The span from the parameter name through its final modifier.
    pub span: SourceSpan,
}

/// A parameter declared by a CLIENT function.
///
/// CLIENT parameters use the same lossless declaration shape as SERVER
/// parameters. The compiler applies the closed CLIENT rule that rejects a
/// non-empty list in this slice.
pub type ClientFunctionParameter = ServerFunctionParameter;

/// A named field returned by a `ROWS (...)` server function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowsColumnDeclaration {
    /// The field name as written in source.
    pub name: NamePart,
    /// The zero-based source order of the field.
    pub order: usize,
    /// The declared field type.
    pub type_specification: TypeSpecification,
    /// The span from the field name through its type.
    pub span: SourceSpan,
}

/// The declared result shape of a function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionReturnType {
    /// One scalar or reference value.
    Single(TypeSpecification),
    /// Zero or more values of one streamed element type.
    Stream {
        /// The streamed element type.
        element: TypeSpecification,
        /// The span from `STREAM` through the closing angle bracket.
        span: SourceSpan,
    },
    /// Zero or more records with the declared fields.
    Rows {
        /// The returned fields in source order.
        columns: Vec<RowsColumnDeclaration>,
        /// The span from `ROWS` through the closing parenthesis.
        span: SourceSpan,
    },
}

/// The security context used to execute a server function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionSecurity {
    /// Execute with the caller's security context.
    Invoker,
    /// Execute with the function owner's security context.
    Definer,
}

/// The transaction behaviour declared by a server function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionTransaction {
    /// Execute as one atomic transaction.
    Atomic,
    /// Execute without writes.
    ReadOnly,
    /// Manage transaction boundaries explicitly.
    Manual,
}

/// The volatility declared by a server function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionVolatility {
    /// The function result is independent of database state.
    Immutable,
    /// The function result is stable for one statement.
    Stable,
    /// The function result can change for each call.
    Volatile,
}

/// One capability required by a function declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilitySpecification {
    /// The capability name as written in source.
    pub name: QualifiedName,
    /// The exact argument source inside the optional parentheses.
    ///
    /// `Some` with an empty slice represents an explicitly empty argument list.
    pub arguments: Option<SourceSlice>,
    /// The span from the capability name through the optional closing parenthesis.
    pub span: SourceSpan,
}

/// The body of a server function.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerFunctionBody {
    /// A parsed Orna relational query retained with its exact source.
    SqlQuery(SqlQueryBody),
    /// A parsed single-row Orna relational insert retained with its exact source.
    SqlInsert(SqlInsertBody),
    /// A parsed single-object Orna relational update retained with its exact source.
    SqlUpdate(SqlUpdateBody),
    /// A parsed single-object Orna relational delete retained with its exact source.
    SqlDelete(SqlDeleteBody),
    /// A parsed closed `SELECT <parameter>` body with no object source.
    NoInputParameterSelect(NoInputParameterSelectBody),
}

impl ServerFunctionBody {
    /// Returns the relational query when this body contains one.
    ///
    /// Callers must use this accessor when they support only query bodies. A
    /// later body kind can then fail closed instead of being treated as a
    /// query.
    #[must_use]
    pub fn as_sql_query(&self) -> Option<&SqlQueryBody> {
        match self {
            Self::SqlQuery(query) => Some(query),
            _ => None,
        }
    }

    /// Returns the relational insert when this body contains one.
    #[must_use]
    pub fn as_sql_insert(&self) -> Option<&SqlInsertBody> {
        match self {
            Self::SqlInsert(insert) => Some(insert),
            _ => None,
        }
    }

    /// Returns the relational update when this body contains one.
    #[must_use]
    pub fn as_sql_update(&self) -> Option<&SqlUpdateBody> {
        match self {
            Self::SqlUpdate(update) => Some(update),
            _ => None,
        }
    }

    /// Returns the relational delete when this body contains one.
    #[must_use]
    pub fn as_sql_delete(&self) -> Option<&SqlDeleteBody> {
        match self {
            Self::SqlDelete(delete) => Some(delete),
            _ => None,
        }
    }

    /// Returns the closed no-input parameter select when this body contains one.
    #[must_use]
    pub fn as_no_input_parameter_select(&self) -> Option<&NoInputParameterSelectBody> {
        match self {
            Self::NoInputParameterSelect(select) => Some(select),
            _ => None,
        }
    }
}

/// The relational query body of a server function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlQueryBody {
    /// The exact source text for the query body, without the declaration terminator.
    pub source: SourceSlice,
    /// The typed Orna query syntax.
    pub query: SelectQuery,
}

/// The closed `SELECT <parameter>` body of a server function.
///
/// This body has no object source, predicate, ordering, or other clause. It
/// is disjoint from [`SelectQuery`], which always requires an object source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoInputParameterSelectBody {
    /// The exact source text for the body, without the declaration terminator.
    pub source: SourceSlice,
    /// The bare parameter identifier selected by the body.
    pub parameter: NamePart,
}

/// The closed value forms supported by SQL mutation bodies.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationValue {
    /// A bare declared server-function parameter name.
    Parameter(NamePart),
    /// A boolean literal, retaining its exact source spelling.
    BooleanLiteral {
        /// The boolean value selected by the source text.
        value: bool,
        /// The exact source spelling of the literal.
        source: SourceSlice,
    },
    /// A null literal, retaining its exact source spelling.
    NullLiteral {
        /// The exact source spelling of the literal.
        source: SourceSlice,
    },
    /// A named record constructed in one SERVER INSERT value position.
    RecordConstructor(RecordConstructor),
}

impl MutationValue {
    /// Return the complete source span for this value.
    pub fn span(&self) -> &SourceSpan {
        match self {
            Self::Parameter(name) => &name.span,
            Self::BooleanLiteral { source, .. } | Self::NullLiteral { source } => &source.span,
            Self::RecordConstructor(constructor) => &constructor.span,
        }
    }
}

/// One lossless named record constructor in a SERVER INSERT value position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordConstructor {
    /// The nominal record type as written before the opening brace.
    pub record_type: QualifiedName,
    /// The constructor fields in source order.
    pub fields: Vec<RecordConstructorField>,
    /// The span from the record type through the closing brace.
    pub span: SourceSpan,
}

/// One named field supplied by a record constructor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordConstructorField {
    /// The field name as written before the colon.
    pub name: NamePart,
    /// The closed source value supplied for the field.
    pub value: RecordConstructorFieldValue,
    /// The span from the field name through its value.
    pub span: SourceSpan,
}

/// The closed field expressions accepted by the first record constructor host.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordConstructorFieldValue {
    /// A bare declared SERVER function parameter name.
    Parameter(NamePart),
    /// A Boolean literal, retaining its exact source spelling.
    BooleanLiteral {
        /// The Boolean value selected by the source text.
        value: bool,
        /// The exact source spelling of the literal.
        source: SourceSlice,
    },
}

impl RecordConstructorFieldValue {
    /// Return the complete source span for this field value.
    pub fn span(&self) -> &SourceSpan {
        match self {
            Self::Parameter(parameter) => &parameter.span,
            Self::BooleanLiteral { source, .. } => &source.span,
        }
    }
}

/// The value forms supported by a single-row SQL insert body.
pub type InsertValue = MutationValue;

/// A parsed single-row `INSERT` body of a server function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlInsertBody {
    /// The exact source text for the insert body, without the declaration terminator.
    pub source: SourceSlice,
    /// The parsed insert statement.
    pub insert: InsertStatement,
}

/// One parsed single-row `INSERT` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertStatement {
    /// The object type named after `INSERT INTO`.
    pub target_object: QualifiedName,
    /// The mandatory alias written after `AS`.
    pub target_alias: NamePart,
    /// The target fields in their positional source order.
    pub target_fields: Vec<NamePart>,
    /// The one row of values in positional source order.
    pub values: Vec<InsertValue>,
    /// The alias written inside the `RETURNING REF(...)` expression.
    pub returning_alias: NamePart,
    /// The span from `REF` through the closing `RETURNING REF(...)` parenthesis.
    pub returning_ref_span: SourceSpan,
    /// The span from `INSERT` through the closing `RETURNING REF(...)` parenthesis.
    pub span: SourceSpan,
}

/// A parsed single-object `UPDATE` body of a server function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlUpdateBody {
    /// The exact source text for the update body, without the declaration terminator.
    pub source: SourceSlice,
    /// The parsed update statement.
    pub update: UpdateStatement,
}

/// One target-field assignment in an `UPDATE` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateAssignment {
    /// The unqualified target field.
    pub target_field: NamePart,
    /// The assigned value.
    pub value: MutationValue,
    /// The span from the target field through the assigned value.
    pub span: SourceSpan,
}

/// One parsed identity-selected `UPDATE` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateStatement {
    /// The object type named after `UPDATE`.
    pub target_object: QualifiedName,
    /// The mandatory alias written after `AS`.
    pub target_alias: NamePart,
    /// The target-field assignments in source order.
    pub assignments: Vec<UpdateAssignment>,
    /// The alias written inside the selector `REF(...)` expression.
    pub selector_alias: NamePart,
    /// The declared function parameter that supplies the selected object identity.
    pub selector_parameter: NamePart,
    /// The span from the selector `REF` through the declared selector parameter.
    pub selector_equality_span: SourceSpan,
    /// The span from selector `REF` through its closing parenthesis.
    pub selector_ref_span: SourceSpan,
    /// The alias written inside the `RETURNING REF(...)` expression.
    pub returning_alias: NamePart,
    /// The span from `REF` through the closing `RETURNING REF(...)` parenthesis.
    pub returning_ref_span: SourceSpan,
    /// The span from `UPDATE` through the closing `RETURNING REF(...)` parenthesis.
    pub span: SourceSpan,
}

/// A parsed single-object `DELETE` body of a server function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlDeleteBody {
    /// The exact source text for the delete body, without the declaration terminator.
    pub source: SourceSlice,
    /// The parsed delete statement.
    pub delete: DeleteStatement,
}

/// One parsed identity-selected `DELETE` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteStatement {
    /// The object type named after `DELETE FROM`.
    pub target_object: QualifiedName,
    /// The mandatory alias written after `AS`.
    pub target_alias: NamePart,
    /// The alias written inside the selector `REF(...)` expression.
    pub selector_alias: NamePart,
    /// The declared function parameter that supplies the selected object identity.
    pub selector_parameter: NamePart,
    /// The span from the selector `REF` through the declared selector parameter.
    pub selector_equality_span: SourceSpan,
    /// The span from selector `REF` through its closing parenthesis.
    pub selector_ref_span: SourceSpan,
    /// The exact `TRUE` source written after `RETURNING`.
    pub returning_true: SourceSlice,
    /// The span from `DELETE` through the `RETURNING TRUE` literal.
    pub span: SourceSpan,
}

/// A parsed `CREATE SERVER FUNCTION` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerFunctionDeclaration {
    /// The declared function name.
    pub name: QualifiedName,
    /// The function parameters in source order.
    pub parameters: Vec<ServerFunctionParameter>,
    /// The declared result shape.
    pub return_type: FunctionReturnType,
    /// The optional execution security mode.
    pub security: Option<FunctionSecurity>,
    /// The optional transaction mode.
    pub transaction: Option<FunctionTransaction>,
    /// The optional volatility mode.
    pub volatility: Option<FunctionVolatility>,
    /// The capabilities required by the function, in source order.
    pub capabilities: Vec<CapabilitySpecification>,
    /// The retained server function body.
    pub body: ServerFunctionBody,
    /// The declaration span, including its terminating semicolon.
    pub span: SourceSpan,
}

/// The closed body of a CLIENT function.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientFunctionBody {
    /// A Boolean literal returned by the function.
    BooleanLiteral {
        /// The Boolean value selected by the source text.
        value: bool,
        /// The exact literal spelling and source span.
        source: SourceSlice,
    },
    /// A closed CLIENT expression evaluated by the local client.
    Expression {
        /// The checked expression returned by the function.
        expression: ClientExpression,
    },
    /// A closed CLIENT expression returned by the short `RETURN` form.
    ///
    /// This remains distinct from [`Self::Expression`] so the compiler can
    /// enforce the public UI rule that `AS expression` is not a CLIENT UI
    /// body, while both forms share the same closed expression grammar.
    ReturnExpression {
        /// The checked expression returned by the function.
        expression: ClientExpression,
    },
    /// An external function body declared only by its runtime contract.
    ExternalContract {
        /// The exact contract identity spelling and source span.
        identity: SourceSlice,
    },
    /// A closed state or procedural local-binding block with one return statement.
    ///
    /// The block body accepts only `STATE` declarations or `LET` local
    /// bindings before `BEGIN` and exactly one `RETURN` statement before
    /// `END`.
    StateBlock(ClientStateBlockBody),
}

impl ClientFunctionBody {
    /// Return the Boolean literal when this body contains one.
    #[must_use]
    pub fn as_boolean_literal(&self) -> Option<(bool, &SourceSlice)> {
        match self {
            Self::BooleanLiteral { value, source } => Some((*value, source)),
            Self::Expression { .. }
            | Self::ReturnExpression { .. }
            | Self::ExternalContract { .. }
            | Self::StateBlock(_) => None,
        }
    }

    /// Return the closed expression when this body contains one.
    #[must_use]
    pub fn as_expression(&self) -> Option<&ClientExpression> {
        match self {
            Self::Expression { expression } | Self::ReturnExpression { expression } => {
                Some(expression)
            }
            Self::BooleanLiteral { .. } | Self::ExternalContract { .. } | Self::StateBlock(_) => {
                None
            }
        }
    }

    /// Return the contract identity when this body is external.
    #[must_use]
    pub fn as_external_contract(&self) -> Option<&SourceSlice> {
        match self {
            Self::ExternalContract { identity } => Some(identity),
            Self::BooleanLiteral { .. }
            | Self::Expression { .. }
            | Self::ReturnExpression { .. }
            | Self::StateBlock(_) => None,
        }
    }

    /// Return the parsed state block when this body declares one.
    #[must_use]
    pub fn as_state_block(&self) -> Option<&ClientStateBlockBody> {
        match self {
            Self::StateBlock(block) => Some(block),
            Self::BooleanLiteral { .. }
            | Self::Expression { .. }
            | Self::ReturnExpression { .. }
            | Self::ExternalContract { .. } => None,
        }
    }
}

/// The declared scope of one CLIENT state slot (work ADR 0069).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateScope {
    /// State private to one mounted function instance.
    Local,
    /// State retained for the client invocation session.
    Session,
    /// State associated with the authenticated principal.
    User,
}

impl ClientFunctionBody {
    /// Return the parsed procedural statements in a CLIENT block.
    #[must_use]
    pub fn procedural_statements(&self) -> Option<&[ClientProceduralStatement]> {
        match self {
            Self::StateBlock(block) => Some(&block.statements),
            Self::BooleanLiteral { .. }
            | Self::Expression { .. }
            | Self::ReturnExpression { .. }
            | Self::ExternalContract { .. } => None,
        }
    }
}

/// The initial value declaration for one CLIENT state slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateDefault {
    /// No DEFAULT clause was written.
    Unset,
    /// The slot starts with an explicit null value.
    Null,
    /// The slot starts with a closed CLIENT expression value.
    Expression(ClientExpression),
}

/// One parsed `STATE` declaration inside a CLIENT state block.
///
/// The declaration follows the canonical shape
/// `STATE identifier type_spec [SCOPE (LOCAL | SESSION | USER)]
/// [DEFAULT expression] ;`. An omitted scope means `LOCAL`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateDeclaration {
    /// The declared state name as written.
    pub name: NamePart,
    /// The declared state type.
    pub type_specification: TypeSpecification,
    /// The declared scope; `StateScope::Local` when the clause is omitted.
    pub scope: StateScope,
    /// The declared initial value.
    pub default: StateDefault,
    /// The span from `STATE` through the terminating semicolon.
    pub span: SourceSpan,
}
/// A closed state/procedural CLIENT block with declarations, procedural
/// statements, and an optional terminal return.
///
/// The block body accepts `STATE` declarations or `LET` local bindings before
/// `BEGIN`, zero or more procedural statements after `BEGIN`, and an optional
/// terminal `RETURN` statement before `END`. A simple legacy block keeps its
/// terminal return in `return_expression`; nested and early returns remain in
/// `statements`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientStateBlockBody {
    /// The state declarations in source order.
    pub states: Vec<StateDeclaration>,
    /// The local resource bindings in source order.
    pub locals: Vec<ClientLocalBinding>,
    /// The procedural statements after `BEGIN`, in source order.
    pub statements: Vec<ClientProceduralStatement>,
    /// The parsed terminal return expression, when the statement names one.
    ///
    /// A bare terminal `RETURN;` is represented by `None`, as is a block that
    /// has no terminal return and relies on control-flow returns.
    pub return_expression: Option<ClientExpression>,
    /// The span from the `IS` keyword through the closing `END`.
    pub span: SourceSpan,
}

/// One local binding in a procedural CLIENT block.
///
/// The type source remains lossless here because resource result descriptors
/// are resolved by the compiler, while the existing syntax type grammar does
/// not represent `Resource<TABLE(...)>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientLocalBinding {
    /// The local name as written.
    pub name: NamePart,
    /// The exact declared type source between the name and `:=`.
    pub type_source: SourceSlice,
    /// The resource constructor expression.
    pub expression: ClientExpression,
    /// The span from `LET` through the terminating semicolon.
    pub span: SourceSpan,
}

/// One procedural statement after `BEGIN` in a CLIENT block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientProceduralStatement {
    /// A local binding with an optional declared type.
    Let(ClientLetStatement),
    /// An assignment to a local or state name.
    Assignment(ClientAssignmentStatement),
    /// An early return from the enclosing CLIENT function.
    Return(ClientReturnStatement),
    /// A conditional statement with optional ELSIF and ELSE branches.
    If(ClientIfStatement),
    /// A condition-controlled loop.
    While(ClientWhileStatement),
}

/// One procedural `LET` statement after `BEGIN`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientLetStatement {
    /// The local name as written.
    pub name: NamePart,
    /// The optional exact declared type source between the name and `:=`.
    pub type_source: Option<SourceSlice>,
    /// The initializer expression.
    pub expression: ClientExpression,
    /// The span from `LET` through the terminating semicolon.
    pub span: SourceSpan,
}

/// One procedural assignment statement after `BEGIN`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientAssignmentStatement {
    /// The assigned name as written.
    pub target: NamePart,
    /// The assigned expression.
    pub expression: ClientExpression,
    /// The span from the target through the terminating semicolon.
    pub span: SourceSpan,
}

/// One procedural `RETURN [expression];` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientReturnStatement {
    /// The optional value returned by this statement.
    pub expression: Option<ClientExpression>,
    /// The span from `RETURN` through its terminating semicolon.
    pub span: SourceSpan,
}

impl ClientReturnStatement {
    /// Return the optional value expression.
    #[must_use]
    pub fn expression(&self) -> Option<&ClientExpression> {
        self.expression.as_ref()
    }

    /// Return the complete source span of this statement.
    #[must_use]
    pub const fn span(&self) -> &SourceSpan {
        &self.span
    }
}

/// One conditional branch in a CLIENT `IF` statement.
///
/// The condition is always present for a `THEN` or `ELSIF` branch. The
/// [`ClientIfStatement::else_statements`] collection represents the optional
/// ELSE branch, so this struct does not need a sentinel condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientIfBranch {
    /// The branch condition.
    pub condition: ClientExpression,
    /// The statements executed when this branch is selected.
    pub statements: Vec<ClientProceduralStatement>,
    /// The span from `IF`/`ELSIF` through the branch body.
    pub span: SourceSpan,
}

impl ClientIfBranch {
    /// Return the branch condition.
    #[must_use]
    pub const fn condition(&self) -> &ClientExpression {
        &self.condition
    }

    /// Return the branch statements in source order.
    #[must_use]
    pub fn statements(&self) -> &[ClientProceduralStatement] {
        &self.statements
    }

    /// Return the complete source span of this branch.
    #[must_use]
    pub const fn span(&self) -> &SourceSpan {
        &self.span
    }
}

/// One procedural `IF expression THEN ...` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientIfStatement {
    /// The condition of the initial `IF` branch.
    pub condition: ClientExpression,
    /// The statements in the initial `THEN` branch.
    pub then_statements: Vec<ClientProceduralStatement>,
    /// The subsequent `ELSIF` branches in source order.
    pub elsif_branches: Vec<ClientIfBranch>,
    /// The optional statements in the `ELSE` branch.
    pub else_statements: Option<Vec<ClientProceduralStatement>>,
    /// The span from `IF` through `END IF;`.
    pub span: SourceSpan,
}

impl ClientIfStatement {
    /// Return the initial `IF` condition.
    #[must_use]
    pub const fn condition(&self) -> &ClientExpression {
        &self.condition
    }

    /// Return the initial `THEN` statements in source order.
    #[must_use]
    pub fn then_statements(&self) -> &[ClientProceduralStatement] {
        &self.then_statements
    }

    /// Return the `ELSIF` branches in source order.
    #[must_use]
    pub fn elsif_branches(&self) -> &[ClientIfBranch] {
        &self.elsif_branches
    }

    /// Return the optional `ELSE` statements in source order.
    #[must_use]
    pub fn else_statements(&self) -> Option<&[ClientProceduralStatement]> {
        self.else_statements.as_deref()
    }

    /// Return the complete source span of this statement.
    #[must_use]
    pub const fn span(&self) -> &SourceSpan {
        &self.span
    }
}

/// One procedural `WHILE expression LOOP ...` statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientWhileStatement {
    /// The loop condition.
    pub condition: ClientExpression,
    /// The statements executed for each iteration.
    pub body: Vec<ClientProceduralStatement>,
    /// The span from `WHILE` through `END LOOP;`.
    pub span: SourceSpan,
}

impl ClientWhileStatement {
    /// Return the loop condition.
    #[must_use]
    pub const fn condition(&self) -> &ClientExpression {
        &self.condition
    }

    /// Return the loop body in source order.
    #[must_use]
    pub fn body(&self) -> &[ClientProceduralStatement] {
        &self.body
    }

    /// Return the complete source span of this statement.
    #[must_use]
    pub const fn span(&self) -> &SourceSpan {
        &self.span
    }
}

/// A unary operator in a typed CLIENT expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientUnaryOperator {
    /// Numeric unary plus (`+expression`).
    Plus,
    /// Numeric negation (`-expression`).
    Minus,
    /// Boolean negation (`NOT expression`).
    Not,
}

impl ClientUnaryOperator {
    /// Return the canonical source spelling of this operator.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Plus => "+",
            Self::Minus => "-",
            Self::Not => "NOT",
        }
    }
}

/// A unary CLIENT expression with its exact source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientUnaryExpression {
    /// The operator applied to the operand.
    pub operator: ClientUnaryOperator,
    /// The operand expression.
    pub expression: Box<ClientExpression>,
    /// The span from the operator through the operand.
    pub span: SourceSpan,
}

impl ClientUnaryExpression {
    /// Return the unary operator.
    #[must_use]
    pub const fn operator(&self) -> ClientUnaryOperator {
        self.operator
    }

    /// Return the operand expression.
    #[must_use]
    pub const fn operand(&self) -> &ClientExpression {
        &self.expression
    }

    /// Return the complete source span of this expression.
    #[must_use]
    pub const fn span(&self) -> &SourceSpan {
        &self.span
    }
}

/// A binary CLIENT expression with its exact source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientBinaryExpression {
    /// The left operand.
    pub left: Box<ClientExpression>,
    /// The operator between the operands.
    pub operator: ClientBinaryOperator,
    /// The right operand.
    pub right: Box<ClientExpression>,
    /// The span from the left operand through the right operand.
    pub span: SourceSpan,
}

impl ClientBinaryExpression {
    /// Return the left operand.
    #[must_use]
    pub const fn left(&self) -> &ClientExpression {
        &self.left
    }

    /// Return the binary operator.
    #[must_use]
    pub const fn operator(&self) -> ClientBinaryOperator {
        self.operator
    }

    /// Return the right operand.
    #[must_use]
    pub const fn right(&self) -> &ClientExpression {
        &self.right
    }

    /// Return the complete source span of this expression.
    #[must_use]
    pub const fn span(&self) -> &SourceSpan {
        &self.span
    }
}

/// A binary operator in a typed CLIENT expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientBinaryOperator {
    /// Integer addition.
    Add,
    /// Integer subtraction.
    Subtract,
    /// Integer multiplication.
    Multiply,
    /// Integer division.
    Divide,
    /// Integer remainder.
    Modulo,
    /// Equality comparison.
    Equal,
    /// Inequality comparison.
    NotEqual,
    /// Less-than comparison.
    LessThan,
    /// Greater-than comparison.
    GreaterThan,
    /// Less-than-or-equal comparison.
    LessThanOrEqual,
    /// Greater-than-or-equal comparison.
    GreaterThanOrEqual,
    /// Short-circuit Boolean conjunction.
    And,
    /// Short-circuit Boolean disjunction.
    Or,
}

impl ClientBinaryOperator {
    /// Return the canonical source spelling of this operator.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Subtract => "-",
            Self::Multiply => "*",
            Self::Divide => "/",
            Self::Modulo => "%",
            Self::Equal => "=",
            Self::NotEqual => "!=",
            Self::LessThan => "<",
            Self::GreaterThan => ">",
            Self::LessThanOrEqual => "<=",
            Self::GreaterThanOrEqual => ">=",
            Self::And => "AND",
            Self::Or => "OR",
        }
    }
}

/// One closed CLIENT expression in the ADR 0068 expression surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientExpression {
    /// A call to one CLIENT function with bound arguments.
    Call {
        /// The called function's qualified name as written.
        callee: QualifiedName,
        /// The call arguments in source order.
        arguments: Vec<ClientCallArgument>,
        /// The span from the callee through the closing parenthesis.
        span: SourceSpan,
    },
    /// A single-quoted text literal with doubled-quote escaping.
    StringLiteral {
        /// The unescaped text value.
        value: String,
        /// The exact literal spelling and source span.
        source: SourceSlice,
    },
    /// A non-negative integer literal.
    IntegerLiteral {
        /// The parsed integer value.
        value: i64,
        /// The exact literal spelling and source span.
        source: SourceSlice,
    },
    /// A Boolean literal.
    BooleanLiteral {
        /// The Boolean value selected by the source text.
        value: bool,
        /// The exact literal spelling and source span.
        source: SourceSlice,
    },
    /// A read of one declared parameter.
    ParameterRead {
        /// The parameter name as written.
        parameter: NamePart,
    },
    /// A read of one procedural local binding.
    LocalRead {
        /// The local name as written.
        local: NamePart,
    },
    /// A path from one declared parameter through object fields.
    FieldPath {
        /// The parameter at the start of the path.
        root: NamePart,
        /// The fields selected from the parameter in source order.
        members: Vec<NamePart>,
        /// The span from the root through the final field.
        span: SourceSpan,
    },
    /// A suspension over one closed CLIENT expression.
    Await {
        /// The expression whose resource result is awaited.
        expression: Box<ClientExpression>,
        /// The span from `AWAIT` through the awaited expression.
        span: SourceSpan,
    },
    /// A left-associative text concatenation.
    Concat {
        /// The expression to the left of `||`.
        left: Box<ClientExpression>,
        /// The expression to the right of `||`.
        right: Box<ClientExpression>,
        /// The span from the left expression through the right expression.
        span: SourceSpan,
    },
    /// A unary arithmetic or Boolean expression.
    Unary(ClientUnaryExpression),
    /// A typed arithmetic, comparison, or Boolean expression.
    Binary(ClientBinaryExpression),
    /// A parenthesized expression retained for exact source spans.
    Parenthesized {
        /// The expression inside the parentheses.
        expression: Box<ClientExpression>,
        /// The span from `(` through `)`.
        span: SourceSpan,
    },
}

impl ClientExpression {
    /// Return the complete source span for this expression.
    #[must_use]
    pub fn span(&self) -> &SourceSpan {
        match self {
            Self::Call { span, .. }
            | Self::FieldPath { span, .. }
            | Self::Await { span, .. }
            | Self::Concat { span, .. }
            | Self::Parenthesized { span, .. } => span,
            Self::Unary(unary) => &unary.span,
            Self::Binary(binary) => &binary.span,
            Self::StringLiteral { source, .. }
            | Self::IntegerLiteral { source, .. }
            | Self::BooleanLiteral { source, .. } => &source.span,
            Self::ParameterRead { parameter } => &parameter.span,
            Self::LocalRead { local } => &local.span,
        }
    }
}

/// One argument bound to a CLIENT expression call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientCallArgument {
    /// The named parameter when the argument is written `name => value`.
    pub name: Option<NamePart>,
    /// The argument value expression.
    pub value: ClientExpression,
    /// The span from the argument name (when present) through the value.
    pub span: SourceSpan,
}

/// A parsed `CREATE CLIENT FUNCTION` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientFunctionDeclaration {
    /// The declared function name.
    pub name: QualifiedName,
    /// The function parameters in source order.
    pub parameters: Vec<ClientFunctionParameter>,
    /// The complete parenthesised parameter-list span.
    pub parameter_list_span: SourceSpan,
    /// The declared result shape retained for semantic checking.
    pub return_type: FunctionReturnType,
    /// Whether the declaration used the `CREATE EXTERNAL CLIENT FUNCTION` form.
    pub external: bool,
    /// The exact `RUNTIME CONTRACT '<identity>'` clause, when present.
    pub runtime_contract: Option<SourceSlice>,
    /// The capabilities required by the function, in source order.
    pub capabilities: Vec<CapabilitySpecification>,
    /// The retained CLIENT function body.
    pub body: ClientFunctionBody,
    /// The declaration span, including its terminating semicolon.
    pub span: SourceSpan,
}

/// The action for a reference when its target is deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnDeletePolicy {
    /// Reject deletion while the reference exists.
    Restrict,
    /// Set the reference field to null when the target is deleted.
    SetNull,
    /// Delete the referencing object when the target is deleted.
    Cascade,
}

/// A field in an object type declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectFieldDeclaration {
    /// The field name as written in source.
    pub name: NamePart,
    /// The zero-based source order of the field within its object type.
    pub order: usize,
    /// The type written for the field.
    pub type_specification: TypeSpecification,
    /// Whether the field may be null. Fields are nullable unless `NOT NULL` appears.
    pub nullable: bool,
    /// Whether the field has a uniqueness constraint.
    pub unique: bool,
    /// The unparsed default expression source, if one was declared.
    pub default_expression: Option<SourceSlice>,
    /// The reference delete action, if one was declared.
    pub on_delete: Option<OnDeletePolicy>,
    /// The documentation text declared by a `DOCUMENTATION` modifier.
    pub documentation: Option<SourceSlice>,
    /// The span from the field name through its final modifier.
    pub span: SourceSpan,
}

/// A parsed `CREATE TYPE ... AS OBJECT` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectTypeDeclaration {
    /// The declared object type name.
    pub name: QualifiedName,
    /// The object fields in source order.
    pub fields: Vec<ObjectFieldDeclaration>,
    /// Whether the `FINAL` modifier was declared.
    pub final_type: bool,
    /// The documentation text declared by a `DOCUMENTATION` modifier.
    pub documentation: Option<SourceSlice>,
    /// The declaration span, including its terminating semicolon.
    pub span: SourceSpan,
}

/// One label in a parsed `CREATE TYPE ... AS ENUM` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumLabelDeclaration {
    /// The exact string literal, including apostrophes and source escaping.
    pub literal: SourceSlice,
}

/// A parsed `CREATE TYPE ... AS ENUM` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumTypeDeclaration {
    /// The declared enum type name.
    pub name: QualifiedName,
    /// The enum labels in declaration order.
    pub labels: Vec<EnumLabelDeclaration>,
    /// The declaration span, including its terminating semicolon.
    pub span: SourceSpan,
}

/// A field in a parsed named record value type declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueFieldDeclaration {
    /// The field name as written in source.
    pub name: NamePart,
    /// The zero-based source order of the field within its record type.
    pub order: usize,
    /// The type written for the field.
    pub type_specification: TypeSpecification,
    /// Reserved for a future value-field documentation modifier; currently always `None`.
    pub documentation: Option<SourceSlice>,
    /// The span from the field name through its type specification.
    pub span: SourceSpan,
}

/// A parsed `CREATE TYPE ... AS VALUE (...) IMMUTABLE PERSISTABLE` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordValueTypeDeclaration {
    /// The declared record value type name.
    pub name: QualifiedName,
    /// The record fields in source order.
    pub fields: Vec<ValueFieldDeclaration>,
    /// The span of the required `IMMUTABLE` keyword.
    pub immutable_span: SourceSpan,
    /// The span of the required `PERSISTABLE` keyword.
    pub persistable_span: SourceSpan,
    /// The documentation text declared by a `DOCUMENTATION` modifier.
    pub documentation: Option<SourceSlice>,
    /// The declaration span, including its terminating semicolon.
    pub span: SourceSpan,
}

/// The persistence selected for a primitive value type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimitiveValueTypePersistence {
    /// `PERSISTABLE` was written.
    Persistable,
    /// `TRANSIENT` was written.
    Transient,
}

/// A parsed privileged `CREATE TYPE ... AS VALUE PRIMITIVE` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimitiveValueTypeDeclaration {
    /// The declared value type name.
    pub name: QualifiedName,
    /// The exact kernel contract string literal.
    pub kernel_contract: SourceSlice,
    /// The span of the `KERNEL CONTRACT` modifier.
    pub kernel_contract_modifier_span: SourceSpan,
    /// The selected persistence behaviour.
    pub persistence: PrimitiveValueTypePersistence,
    /// The span of the persistence keyword.
    pub persistence_span: SourceSpan,
    /// The documentation text declared by a `DOCUMENTATION` modifier.
    pub documentation: Option<SourceSlice>,
    /// The declaration span, including its terminating semicolon.
    pub span: SourceSpan,
}

/// A parsed privileged `CREATE TYPE ... AS VALUE OPAQUE` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpaqueValueTypeDeclaration {
    /// The declared opaque value type name.
    pub name: QualifiedName,
    /// The exact codec contract string literal, including apostrophes.
    pub kernel_contract: SourceSlice,
    /// The span of the required `OPAQUE` keyword.
    pub opaque_span: SourceSpan,
    /// The span of the `KERNEL CONTRACT` modifier.
    pub kernel_contract_modifier_span: SourceSpan,
    /// The span of the required `IMMUTABLE` keyword.
    pub immutable_span: SourceSpan,
    /// The span of the required `TRANSIENT` keyword.
    pub transient_span: SourceSpan,
    /// The documentation text declared by a `DOCUMENTATION` modifier.
    pub documentation: Option<SourceSlice>,
    /// The declaration span, including its terminating semicolon.
    pub span: SourceSpan,
}

/// The destination selected by a privileged type export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeExportTarget {
    /// A qualified type binding after `AS`.
    Qualified {
        /// The qualified target type name.
        name: QualifiedName,
    },
    /// A prelude type binding after `TO PRELUDE AS`.
    Prelude {
        /// The unquoted words that form the prelude type name.
        words: Vec<NamePart>,
        /// The span from the first prelude word through the final word.
        name_span: SourceSpan,
        /// The span of the `TO PRELUDE` modifier.
        modifier_span: SourceSpan,
    },
}

/// A parsed privileged `EXPORT TYPE` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeExportDeclaration {
    /// The primary type that this declaration exports.
    pub source_type: QualifiedName,
    /// The target binding selected by the declaration.
    pub target: TypeExportTarget,
    /// The declaration span, including its terminating semicolon.
    pub span: SourceSpan,
}

/// A parsed `ALTER TYPE ... RENAME FIELD ... TO ...` declaration.
///
/// This declaration records source transition evidence. The compiler performs
/// semantic validation and identity binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldRenameDeclaration {
    /// The object type that owns the renamed field.
    pub type_name: QualifiedName,
    /// The field name in the expected base catalogue.
    pub old_field_name: NamePart,
    /// The field name in the candidate declaration.
    pub new_field_name: NamePart,
    /// The declaration span, including its terminating semicolon.
    pub span: SourceSpan,
}

/// A private wrapper around the lossless Rowan tree.
///
/// It intentionally exposes text, rather than Rowan types, as the public CST
/// boundary. This keeps the parser implementation replaceable.
#[derive(Clone)]
pub struct SyntaxTree {
    root: rowan::SyntaxNode<parser::OrnaLanguage>,
}

impl SyntaxTree {
    /// Return the exact source text represented by this tree.
    pub fn text(&self) -> String {
        self.root.to_string()
    }

    /// Return the private Rowan root for intra-crate classification.
    pub(crate) fn root(&self) -> &rowan::SyntaxNode<parser::OrnaLanguage> {
        &self.root
    }
}

impl fmt::Debug for SyntaxTree {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("SyntaxTree").finish_non_exhaustive()
    }
}

/// The output from parsing one source unit.
#[derive(Debug, Clone)]
pub struct Parse {
    syntax: SyntaxTree,
    diagnostics: Vec<Diagnostic>,
    schemas: Vec<SchemaDeclaration>,
    object_types: Vec<ObjectTypeDeclaration>,
    enum_types: Vec<EnumTypeDeclaration>,
    record_value_types: Vec<RecordValueTypeDeclaration>,
    primitive_value_types: Vec<PrimitiveValueTypeDeclaration>,
    opaque_value_types: Vec<OpaqueValueTypeDeclaration>,
    type_exports: Vec<TypeExportDeclaration>,
    field_renames: Vec<FieldRenameDeclaration>,
    server_functions: Vec<ServerFunctionDeclaration>,
    client_functions: Vec<ClientFunctionDeclaration>,
}

impl Parse {
    /// Return the lossless CST.
    pub fn syntax(&self) -> &SyntaxTree {
        &self.syntax
    }

    /// Return all lexical and syntactic diagnostics in source order.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Return successfully parsed schema declarations in source order.
    pub fn schemas(&self) -> &[SchemaDeclaration] {
        &self.schemas
    }

    /// Return successfully parsed object type declarations in source order.
    pub fn object_types(&self) -> &[ObjectTypeDeclaration] {
        &self.object_types
    }

    /// Return successfully parsed enum type declarations in source order.
    pub fn enum_types(&self) -> &[EnumTypeDeclaration] {
        &self.enum_types
    }

    /// Return successfully parsed record value type declarations in source order.
    pub fn record_value_types(&self) -> &[RecordValueTypeDeclaration] {
        &self.record_value_types
    }

    /// Return successfully parsed primitive value type declarations in source order.
    pub fn primitive_value_types(&self) -> &[PrimitiveValueTypeDeclaration] {
        &self.primitive_value_types
    }

    /// Return successfully parsed opaque value type declarations in source order.
    pub fn opaque_value_types(&self) -> &[OpaqueValueTypeDeclaration] {
        &self.opaque_value_types
    }

    /// Return successfully parsed type export declarations in source order.
    pub fn type_exports(&self) -> &[TypeExportDeclaration] {
        &self.type_exports
    }

    /// Return successfully parsed field rename declarations in source order.
    pub fn field_renames(&self) -> &[FieldRenameDeclaration] {
        &self.field_renames
    }

    /// Return successfully parsed server function declarations in source order.
    pub fn server_functions(&self) -> &[ServerFunctionDeclaration] {
        &self.server_functions
    }

    /// Return successfully parsed CLIENT function declarations in source order.
    pub fn client_functions(&self) -> &[ClientFunctionDeclaration] {
        &self.client_functions
    }

    /// Return context-aware highlight tokens for this source unit.
    ///
    /// The classification walks this unit's lossless CST, so declaration
    /// names are recognised even in partially edited source.
    pub fn highlight(&self) -> Vec<HighlightToken> {
        highlight::highlight_tree(&self.syntax)
    }
}

/// Parse one Orna source unit.
///
/// The parser recognises schema declarations, object, enum, record value, and
/// primitive value type declarations, type export declarations, field rename
/// declarations, and function declarations. It keeps all source bytes in its
/// CST, including bytes in malformed statements.
pub fn parse(source: &str) -> Parse {
    parser::parse(source)
}

#[cfg(test)]
mod tests;
