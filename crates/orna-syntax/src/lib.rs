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
mod tests {
    use crate::parser::SyntaxKind;

    use super::{
        ClientExpression, ClientFunctionBody, ClientProceduralStatement, FunctionReturnType,
        FunctionSecurity, FunctionTransaction, FunctionVolatility, InsertValue, MutationValue,
        NullOrdering, OnDeletePolicy, OptionTypeSpelling, OrderingDirection,
        PrimitiveValueTypePersistence, QueryExpression, RecordConstructorFieldValue,
        SelectQuantifier, ServerFunctionBody, SourceSpan, StandardLargeObjectKind, StateDefault,
        StateScope, TypeExportTarget, TypeSpecification, parse,
    };

    #[test]
    fn parses_schema_declarations_case_insensitively_without_rewriting_source() {
        let source = "cReAtE sChEmA crm.sales;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.schemas()[0].name.parts[0].text, "crm");
        assert_eq!(parsed.schemas()[0].name.parts[1].text, "sales");
    }

    #[test]
    fn parses_enum_labels_losslessly_in_declaration_order() {
        let source = "CREATE TYPE crm.stage AS ENUM (\n    'lead', /* keep */ 'qual''ified',\n    'customer'\n);";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        let declaration = &parsed.enum_types()[0];
        assert_eq!(declaration.name.parts[0].text, "crm");
        assert_eq!(declaration.name.parts[1].text, "stage");
        assert_eq!(
            declaration
                .labels
                .iter()
                .map(|label| label.literal.text.as_str())
                .collect::<Vec<_>>(),
            ["'lead'", "'qual''ified'", "'customer'"]
        );
        assert_eq!(
            declaration.labels[1].literal.span.start,
            source.find("'qual''ified'").unwrap()
        );
        assert_eq!(
            declaration.span,
            SourceSpan {
                start: 0,
                end: source.len()
            }
        );
    }

    #[test]
    fn reports_closed_enum_syntax_diagnostics_without_partial_declarations() {
        let cases = [
            (
                "CREATE TYPE app.stage AS ENUM ();",
                "enum type must declare at least one label",
            ),
            (
                "CREATE TYPE app.stage AS ENUM (lead);",
                "expected a string literal enum label",
            ),
            (
                "CREATE TYPE app.stage AS ENUM ('lead',);",
                "enum type cannot have a trailing comma",
            ),
            (
                "CREATE TYPE app.stage AS ENUM ('lead' 'customer');",
                "expected ',' or ')' after enum label",
            ),
            (
                "CREATE TYPE app.stage AS ENUM ('lead';",
                "expected ')' after enum labels",
            ),
            (
                "CREATE TYPE app.stage AS ENUM ('lead')",
                "expected ';' after enum type declaration",
            ),
        ];

        for (source, message) in cases {
            let parsed = parse(source);
            assert!(parsed.enum_types().is_empty(), "{source}");
            assert_eq!(parsed.diagnostics().len(), 1, "{source}");
            assert_eq!(parsed.diagnostics()[0].message, message, "{source}");
            assert_eq!(parsed.syntax().text(), source, "{source}");
        }
    }

    #[test]
    fn recovers_from_an_invalid_enum_to_a_later_declaration() {
        let source = "CREATE TYPE app.stage AS ENUM ('lead',); CREATE SCHEMA later;";
        let parsed = parse(source);

        assert_eq!(parsed.diagnostics().len(), 1);
        assert!(parsed.enum_types().is_empty());
        assert_eq!(parsed.schemas().len(), 1);
        assert_eq!(parsed.schemas()[0].name.parts[0].text, "later");
    }

    #[test]
    fn parses_immutable_record_value_type_losslessly() {
        let source = "CREATE TYPE example.point AS VALUE (\n    x INT,\n    /* ordinate */ y INTEGER,\n)\nIMMUTABLE\nPERSISTABLE;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        let declaration = &parsed.record_value_types()[0];
        assert_eq!(declaration.name.parts[0].text, "example");
        assert_eq!(declaration.name.parts[1].text, "point");
        assert_eq!(declaration.fields.len(), 2);
        assert_eq!(declaration.fields[0].name.text, "x");
        assert_eq!(declaration.fields[0].order, 0);
        assert_eq!(declaration.fields[1].name.text, "y");
        assert_eq!(declaration.fields[1].order, 1);
        assert_eq!(
            declaration.fields[1].span,
            SourceSpan {
                start: source.find("y INTEGER").unwrap(),
                end: source.find("y INTEGER").unwrap() + "y INTEGER".len(),
            }
        );
        assert_eq!(
            declaration.immutable_span.start,
            source.find("IMMUTABLE").unwrap()
        );
        assert_eq!(
            declaration.immutable_span.end,
            source.find("IMMUTABLE").unwrap() + "IMMUTABLE".len()
        );
        assert_eq!(
            declaration.persistable_span.start,
            source.find("PERSISTABLE").unwrap()
        );
        assert_eq!(
            declaration.persistable_span.end,
            source.find("PERSISTABLE").unwrap() + "PERSISTABLE".len()
        );
        assert_eq!(
            declaration.span,
            SourceSpan {
                start: 0,
                end: source.len()
            }
        );

        let without_trailing_comma =
            parse("CREATE TYPE example.point AS VALUE (x INT) IMMUTABLE PERSISTABLE;");
        assert!(without_trailing_comma.diagnostics().is_empty());
        assert_eq!(
            without_trailing_comma.record_value_types()[0].fields.len(),
            1
        );
    }

    #[test]
    fn reports_closed_record_value_type_diagnostics() {
        let cases = [
            (
                "CREATE TYPE app.empty AS VALUE () IMMUTABLE PERSISTABLE;",
                "record value type must declare at least one field",
                ")",
                false,
            ),
            (
                "CREATE TYPE app.point AS VALUE (x INT y INT) IMMUTABLE PERSISTABLE;",
                "expected ',' or ')' after record value field",
                "y",
                false,
            ),
            (
                "CREATE TYPE app.point AS VALUE (x INT;",
                "expected ')' after record value fields",
                ";",
                false,
            ),
            (
                "CREATE TYPE app.point AS VALUE (x INT) PERSISTABLE IMMUTABLE;",
                "expected keyword IMMUTABLE",
                "PERSISTABLE",
                false,
            ),
            (
                "CREATE TYPE app.point AS VALUE (x INT) IMMUTABLE;",
                "expected keyword PERSISTABLE",
                ";",
                false,
            ),
            (
                "CREATE TYPE app.point AS VALUE (x REF app.object) IMMUTABLE PERSISTABLE;",
                "record value fields cannot use REF",
                "REF",
                false,
            ),
            (
                "CREATE TYPE app.point AS VALUE (x INT NOT NULL) IMMUTABLE PERSISTABLE;",
                "record value fields do not accept modifiers",
                "NOT",
                false,
            ),
            (
                "CREATE TYPE app.point AS VALUE (x INT NULL) IMMUTABLE PERSISTABLE;",
                "record value fields do not accept modifiers",
                "NULL",
                false,
            ),
            (
                "CREATE TYPE app.point AS VALUE (x INT DEFAULT 0) IMMUTABLE PERSISTABLE;",
                "record value fields do not accept modifiers",
                "DEFAULT",
                false,
            ),
            (
                "CREATE TYPE app.point AS VALUE (x INT CHECK true) IMMUTABLE PERSISTABLE;",
                "record value fields do not accept modifiers",
                "CHECK",
                false,
            ),
            (
                "CREATE TYPE app.point AS VALUE (x INT) IMMUTABLE IMMUTABLE PERSISTABLE;",
                "expected keyword PERSISTABLE",
                "IMMUTABLE",
                true,
            ),
            (
                "CREATE TYPE app.point AS VALUE (x INT) IMMUTABLE PERSISTABLE PERSISTABLE;",
                "expected ';' after record value type declaration",
                "PERSISTABLE",
                true,
            ),
            (
                "CREATE TYPE app.point AS VALUE (x INT) IMMUTABLE PERSISTABLE EXTRA;",
                "expected ';' after record value type declaration",
                "EXTRA",
                false,
            ),
            (
                "CREATE TYPE app.point AS VALUE (x INT) IMMUTABLE PERSISTABLE",
                "expected ';' after record value type declaration",
                "",
                false,
            ),
        ];

        for (source, message, offending, use_last_occurrence) in cases {
            let parsed = parse(source);
            assert!(parsed.record_value_types().is_empty(), "{source}");
            assert_eq!(parsed.diagnostics().len(), 1, "{source}");
            assert_eq!(parsed.diagnostics()[0].message, message, "{source}");
            let start = if use_last_occurrence {
                source.rfind(offending).unwrap()
            } else if offending.is_empty() {
                source.len()
            } else {
                source.find(offending).unwrap()
            };
            assert_eq!(
                parsed.diagnostics()[0].span,
                SourceSpan {
                    start,
                    end: start + offending.len(),
                },
                "{source}"
            );
            assert_eq!(parsed.syntax().text(), source, "{source}");
        }
    }

    #[test]
    fn captures_documentation_modifiers() {
        let object_field = "CREATE TYPE app.task AS OBJECT (title TEXT DOCUMENTATION 'the title');";
        let parsed = parse(object_field);
        assert!(parsed.diagnostics().is_empty(), "{object_field}");
        let documentation = parsed.object_types()[0].fields[0]
            .documentation
            .as_ref()
            .expect("field documentation");
        assert_eq!(documentation.text, "'the title'");

        let object_type =
            "CREATE TYPE app.task AS OBJECT (title TEXT) FINAL DOCUMENTATION 'a final task';";
        let parsed = parse(object_type);
        assert!(parsed.diagnostics().is_empty(), "{object_type}");
        let declaration = &parsed.object_types()[0];
        assert!(declaration.final_type);
        assert_eq!(
            declaration
                .documentation
                .as_ref()
                .expect("type documentation")
                .text,
            "'a final task'"
        );

        let value_field =
            "CREATE TYPE app.point AS VALUE (x INT DOCUMENTATION 'the x') IMMUTABLE PERSISTABLE;";
        let parsed = parse(value_field);
        assert!(parsed.record_value_types().is_empty(), "{value_field}");
        assert_eq!(parsed.diagnostics().len(), 1, "{value_field}");
        assert_eq!(
            parsed.diagnostics()[0].message,
            "record value fields do not accept modifiers"
        );
        let documentation_start = value_field.find("DOCUMENTATION").unwrap();
        assert_eq!(
            parsed.diagnostics()[0].span,
            SourceSpan {
                start: documentation_start,
                end: documentation_start + "DOCUMENTATION".len(),
            }
        );

        let record =
            "CREATE TYPE app.point AS VALUE (x INT) IMMUTABLE PERSISTABLE DOCUMENTATION 'a point';";
        let parsed = parse(record);
        assert!(parsed.diagnostics().is_empty(), "{record}");
        assert_eq!(
            parsed.record_value_types()[0]
                .documentation
                .as_ref()
                .expect("record documentation")
                .text,
            "'a point'"
        );

        let primitive = "CREATE TYPE app.tick AS VALUE PRIMITIVE KERNEL CONTRACT 'k' IMMUTABLE PERSISTABLE DOCUMENTATION 'a primitive';";
        let parsed = parse(primitive);
        assert!(parsed.diagnostics().is_empty(), "{primitive}");
        assert_eq!(
            parsed.primitive_value_types()[0]
                .documentation
                .as_ref()
                .expect("primitive documentation")
                .text,
            "'a primitive'"
        );

        let opaque = "CREATE TYPE app.blob AS VALUE OPAQUE KERNEL CONTRACT 'k' IMMUTABLE TRANSIENT DOCUMENTATION 'an opaque';";
        let parsed = parse(opaque);
        assert!(parsed.diagnostics().is_empty(), "{opaque}");
        assert_eq!(
            parsed.opaque_value_types()[0]
                .documentation
                .as_ref()
                .expect("opaque documentation")
                .text,
            "'an opaque'"
        );

        let parameter = "CREATE SERVER FUNCTION app.overdue (p_before TIMESTAMP DOCUMENTATION 'cutoff') RETURNS BOOL AS SELECT probe.stored FROM app.probe probe;";
        let parsed = parse(parameter);
        assert!(parsed.diagnostics().is_empty(), "{parameter}");
        assert_eq!(
            parsed.server_functions()[0].parameters[0]
                .documentation
                .as_ref()
                .expect("parameter documentation")
                .text,
            "'cutoff'"
        );
    }

    #[test]
    fn recovers_after_invalid_record_value_type() {
        let source = "CREATE TYPE app.point AS VALUE (x INT NOT NULL) IMMUTABLE PERSISTABLE; CREATE SCHEMA later;";
        let parsed = parse(source);

        assert_eq!(parsed.diagnostics().len(), 1);
        assert!(parsed.record_value_types().is_empty());
        assert_eq!(parsed.schemas().len(), 1);
        assert_eq!(parsed.schemas()[0].name.parts[0].text, "later");
    }

    #[test]
    fn parses_persistable_primitive_value_type_losslessly() {
        let source = "CREATE TYPE std.types.BOOLEAN AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.boolean@1' IMMUTABLE PERSISTABLE;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        let declaration = &parsed.primitive_value_types()[0];
        assert_eq!(declaration.name.parts[0].text, "std");
        assert_eq!(
            declaration.kernel_contract.text,
            "'orna.kernel.value.boolean@1'"
        );
        assert_eq!(
            declaration.persistence,
            PrimitiveValueTypePersistence::Persistable
        );
        assert_eq!(
            declaration.kernel_contract_modifier_span.start,
            source.find("KERNEL").unwrap()
        );
        assert_eq!(
            declaration.persistence_span.start,
            source.find("PERSISTABLE").unwrap()
        );
        assert_eq!(declaration.span.end, source.len());
    }

    #[test]
    fn parses_transient_primitive_and_type_exports_losslessly() {
        let source = "CREATE TYPE std.types.VOID AS VALUE PRIMITIVE KERNEL CONTRACT 'orna.kernel.value.void@1' IMMUTABLE TRANSIENT;\n\
            EXPORT TYPE std.types.VOID AS std.VOID;\n\
            EXPORT TYPE std.VOID TO /* binding */ PRELUDE AS CHARACTER  LARGE\nOBJECT;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(
            parsed.primitive_value_types()[0].persistence,
            PrimitiveValueTypePersistence::Transient
        );
        assert!(matches!(
            parsed.type_exports()[0].target,
            TypeExportTarget::Qualified { .. }
        ));
        if let TypeExportTarget::Qualified { name } = &parsed.type_exports()[0].target {
            assert_eq!(name.parts[1].text, "VOID");
        }
        assert!(matches!(
            parsed.type_exports()[1].target,
            TypeExportTarget::Prelude { .. }
        ));
        if let TypeExportTarget::Prelude {
            words,
            name_span,
            modifier_span,
        } = &parsed.type_exports()[1].target
        {
            assert_eq!(
                words
                    .iter()
                    .map(|word| word.text.as_str())
                    .collect::<Vec<_>>(),
                ["CHARACTER", "LARGE", "OBJECT"]
            );
            assert_eq!(name_span.start, source.rfind("CHARACTER").unwrap());
            assert_eq!(
                name_span.end,
                source.rfind("OBJECT").unwrap() + "OBJECT".len()
            );
            assert_eq!(modifier_span.start, source.rfind("TO").unwrap());
        }
    }

    #[test]
    fn parses_opaque_value_type_losslessly() {
        let source = "CREATE TYPE std.example.token AS VALUE OPAQUE KERNEL CONTRACT 'std.example.token@1' IMMUTABLE TRANSIENT;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        let declaration = &parsed.opaque_value_types()[0];
        assert_eq!(declaration.name.parts[0].text, "std");
        assert_eq!(declaration.name.parts[2].text, "token");
        assert_eq!(declaration.kernel_contract.text, "'std.example.token@1'");
        assert_eq!(
            declaration.opaque_span.start,
            source.find("OPAQUE").unwrap()
        );
        assert_eq!(
            declaration.kernel_contract_modifier_span,
            SourceSpan {
                start: source.find("KERNEL").unwrap(),
                end: source.find("CONTRACT").unwrap() + "CONTRACT".len(),
            }
        );
        assert_eq!(
            declaration.immutable_span.start,
            source.find("IMMUTABLE").unwrap()
        );
        assert_eq!(
            declaration.transient_span.start,
            source.find("TRANSIENT").unwrap()
        );
        assert_eq!(declaration.span.end, source.len());
    }

    #[test]
    fn rejects_every_malformed_opaque_value_shape_and_recovers() {
        let cases = [
            (
                "CREATE TYPE std.bad AS VALUE OPAQUE CONTRACT 'std.bad@1' IMMUTABLE TRANSIENT;",
                "expected KERNEL after OPAQUE",
            ),
            (
                "CREATE TYPE std.bad AS VALUE OPAQUE KERNEL 'std.bad@1' IMMUTABLE TRANSIENT;",
                "expected CONTRACT after KERNEL",
            ),
            (
                "CREATE TYPE std.bad AS VALUE OPAQUE KERNEL CONTRACT IMMUTABLE TRANSIENT;",
                "expected a string literal after KERNEL CONTRACT",
            ),
            (
                "CREATE TYPE std.bad AS VALUE OPAQUE KERNEL CONTRACT 'std.bad@1' TRANSIENT;",
                "expected IMMUTABLE after opaque codec contract",
            ),
            (
                "CREATE TYPE std.bad AS VALUE OPAQUE KERNEL CONTRACT 'std.bad@1' IMMUTABLE PERSISTABLE;",
                "expected TRANSIENT after IMMUTABLE",
            ),
            (
                "CREATE TYPE std.bad AS VALUE OPAQUE KERNEL CONTRACT 'std.bad@1' IMMUTABLE TRANSIENT EXTRA;",
                "expected ';' after opaque value type declaration",
            ),
            (
                "CREATE TYPE std.bad AS VALUE OPAQUE KERNEL CONTRACT 'std.bad@1' IMMUTABLE TRANSIENT",
                "expected ';' after opaque value type declaration",
            ),
        ];

        for (invalid, message) in cases {
            let source = format!("{invalid} CREATE SCHEMA later;");
            let parsed = parse(&source);
            assert!(parsed.opaque_value_types().is_empty(), "{invalid}");
            assert_eq!(parsed.diagnostics().len(), 1, "{invalid}");
            assert_eq!(parsed.diagnostics()[0].message, message, "{invalid}");
            assert_eq!(parsed.schemas().len(), 1, "{invalid}");
            assert_eq!(parsed.schemas()[0].name.parts[0].text, "later");
            assert_eq!(parsed.syntax().text(), source);
        }
    }

    #[test]
    fn reports_closed_primitive_and_export_syntax_diagnostics() {
        let cases = [
            (
                "CREATE TYPE app.value AS ;",
                "expected OBJECT, ENUM, or VALUE after AS",
            ),
            (
                "CREATE TYPE app.value AS VALUE ;",
                "expected keyword PRIMITIVE",
            ),
            (
                "CREATE TYPE app.value AS VALUE PRIMITIVE ;",
                "expected keyword KERNEL",
            ),
            (
                "CREATE TYPE app.value AS VALUE PRIMITIVE KERNEL ;",
                "expected keyword CONTRACT",
            ),
            (
                "CREATE TYPE app.value AS VALUE PRIMITIVE KERNEL CONTRACT ;",
                "expected a string literal after KERNEL CONTRACT",
            ),
            (
                "CREATE TYPE app.value AS VALUE PRIMITIVE KERNEL CONTRACT 'app.value@1' ;",
                "expected keyword IMMUTABLE",
            ),
            (
                "CREATE TYPE app.value AS VALUE PRIMITIVE KERNEL CONTRACT 'app.value@1' IMMUTABLE ;",
                "expected PERSISTABLE or TRANSIENT after IMMUTABLE",
            ),
            (
                "CREATE TYPE app.value AS VALUE PRIMITIVE KERNEL CONTRACT 'app.value@1' IMMUTABLE PERSISTABLE",
                "expected ';' after primitive value type declaration",
            ),
            ("EXPORT ;", "expected keyword TYPE"),
            ("EXPORT TYPE ;", "expected a type name after EXPORT TYPE"),
            (
                "EXPORT TYPE app.value ;",
                "expected AS or TO after exported type name",
            ),
            ("EXPORT TYPE app.value TO ;", "expected keyword PRELUDE"),
            ("EXPORT TYPE app.value TO PRELUDE ;", "expected keyword AS"),
            (
                "EXPORT TYPE app.value TO PRELUDE AS ;",
                "expected an unquoted prelude type name after AS",
            ),
            (
                "EXPORT TYPE app.value AS ;",
                "expected a qualified type name after AS",
            ),
            (
                "EXPORT TYPE app.value AS app.binding",
                "expected ';' after type export declaration",
            ),
        ];

        for (source, message) in cases {
            let parsed = parse(source);
            assert_eq!(parsed.diagnostics().len(), 1, "{source}");
            assert_eq!(parsed.diagnostics()[0].message, message, "{source}");
        }
    }

    #[test]
    fn recovers_from_primitive_and_export_errors_to_later_exports() {
        let source = "CREATE TYPE app.value AS VALUE PRIMITIVE KERNEL CONTRACT 'app.value@1' IMMUTABLE;\n\
            EXPORT TYPE app.value AS app.binding;";
        let parsed = parse(source);

        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(
            parsed.diagnostics()[0].message,
            "expected PERSISTABLE or TRANSIENT after IMMUTABLE"
        );
        assert_eq!(parsed.type_exports().len(), 1);
    }

    #[test]
    fn recovers_from_malformed_qualified_and_prelude_exports() {
        let qualified_source = "EXPORT TYPE app.value AS ; CREATE SCHEMA later;";
        let qualified = parse(qualified_source);
        assert_eq!(qualified.diagnostics().len(), 1);
        assert_eq!(
            qualified.diagnostics()[0].message,
            "expected a qualified type name after AS"
        );
        assert_eq!(qualified.schemas().len(), 1);
        assert_eq!(qualified.schemas()[0].name.parts[0].text, "later");

        let prelude_source =
            "EXPORT TYPE app.value TO PRELUDE AS ; EXPORT TYPE app.value AS app.binding;";
        let prelude = parse(prelude_source);
        assert_eq!(prelude.diagnostics().len(), 1);
        assert_eq!(
            prelude.diagnostics()[0].message,
            "expected an unquoted prelude type name after AS"
        );
        assert_eq!(prelude.type_exports().len(), 1);
        assert!(matches!(
            prelude.type_exports()[0].target,
            TypeExportTarget::Qualified { .. }
        ));
    }

    #[test]
    fn recovers_from_missing_object_fields_without_panicking() {
        let source = "CREATE TYPE app.value AS OBJECT ; CREATE SCHEMA later;";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert!(parsed.object_types().is_empty());
        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(
            parsed.diagnostics()[0].message,
            "expected '(' after AS OBJECT"
        );
        assert_eq!(
            parsed.diagnostics()[0].span.start,
            source.find(';').unwrap()
        );
        assert_eq!(parsed.schemas().len(), 1);
        assert_eq!(parsed.schemas()[0].name.parts[0].text, "later");
    }

    #[test]
    fn recovers_missing_server_parameters_at_root_level() {
        let source = "CREATE SERVER FUNCTION app.f ; CREATE SCHEMA later;";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(
            parsed.diagnostics()[0].message,
            "expected '(' after server function name"
        );
        assert_eq!(
            parsed.diagnostics()[0].span.start,
            source.find(';').unwrap()
        );
        assert_eq!(parsed.schemas().len(), 1);
        assert_eq!(parsed.schemas()[0].name.parts[0].text, "later");

        let root = &parsed.syntax().root;
        assert_eq!(root.kind(), SyntaxKind::Root);
        assert_eq!(
            root.children().map(|node| node.kind()).collect::<Vec<_>>(),
            [
                SyntaxKind::CreateServerFunctionStatement,
                SyntaxKind::CreateSchemaStatement,
            ]
        );
    }

    #[test]
    fn preserves_a_create_declaration_after_a_missing_prelude_export_semicolon() {
        let source = "EXPORT TYPE std.X TO PRELUDE AS X CREATE SCHEMA later;";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert!(parsed.type_exports().is_empty());
        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(
            parsed.diagnostics()[0].message,
            "expected ';' after type export declaration"
        );
        let boundary = source.find("CREATE").unwrap();
        assert_eq!(parsed.diagnostics()[0].span.start, boundary);
        assert_eq!(parsed.diagnostics()[0].span.end, boundary + "CREATE".len());
        assert_eq!(parsed.schemas().len(), 1);
        assert_eq!(parsed.schemas()[0].name.parts[0].text, "later");
    }

    #[test]
    fn preserves_a_create_declaration_after_a_missing_prelude_alias() {
        let source = "EXPORT TYPE std.X TO PRELUDE AS CREATE SCHEMA later;";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert!(parsed.type_exports().is_empty());
        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(
            parsed.diagnostics()[0].message,
            "expected an unquoted prelude type name after AS"
        );
        let boundary = source.find("CREATE").unwrap();
        assert_eq!(parsed.diagnostics()[0].span.start, boundary);
        assert_eq!(parsed.diagnostics()[0].span.end, boundary + "CREATE".len());
        assert_eq!(parsed.schemas().len(), 1);
        assert_eq!(parsed.schemas()[0].name.parts[0].text, "later");
    }

    #[test]
    fn preserves_an_export_declaration_after_a_missing_prelude_export_semicolon() {
        let source = "EXPORT TYPE std.X TO PRELUDE AS X EXPORT TYPE std.X AS std.Y;";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(
            parsed.diagnostics()[0].message,
            "expected ';' after type export declaration"
        );
        let boundary = source.rfind("EXPORT").unwrap();
        assert_eq!(parsed.diagnostics()[0].span.start, boundary);
        assert_eq!(parsed.diagnostics()[0].span.end, boundary + "EXPORT".len());
        assert_eq!(parsed.type_exports().len(), 1);
        assert_eq!(parsed.type_exports()[0].source_type.parts[1].text, "X");
        assert!(matches!(
            parsed.type_exports()[0].target,
            TypeExportTarget::Qualified { .. }
        ));
        if let TypeExportTarget::Qualified { name } = &parsed.type_exports()[0].target {
            assert_eq!(name.parts[1].text, "Y");
        }
    }

    #[test]
    fn preserves_an_alter_declaration_after_a_missing_prelude_export_semicolon() {
        let source =
            "EXPORT TYPE std.X TO PRELUDE AS X ALTER TYPE later.item RENAME FIELD old TO new;";
        let parsed = parse(source);

        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(
            parsed.diagnostics()[0].message,
            "expected ';' after type export declaration"
        );
        let boundary = source.find("ALTER").unwrap();
        assert_eq!(parsed.diagnostics()[0].span.start, boundary);
        assert_eq!(parsed.diagnostics()[0].span.end, boundary + "ALTER".len());
        assert_eq!(parsed.field_renames().len(), 1);
        assert_eq!(parsed.field_renames()[0].type_name.parts[0].text, "later");
    }

    #[test]
    fn reports_the_missing_qualified_export_target_at_its_source_span() {
        let source = "EXPORT TYPE app.value AS ;";
        let parsed = parse(source);

        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(
            parsed.diagnostics()[0].message,
            "expected a qualified type name after AS"
        );
        let semicolon = source.find(';').unwrap();
        assert_eq!(parsed.diagnostics()[0].span.start, semicolon);
        assert_eq!(parsed.diagnostics()[0].span.end, semicolon + 1);
    }

    #[test]
    fn preserves_trivia_across_multiple_schema_declarations() {
        let source =
            "-- initial namespace\nCREATE SCHEMA people; /* task data */\nCREATE SCHEMA tasks;\n";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.schemas().len(), 2);
        assert_eq!(parsed.schemas()[1].span.start, 59);
    }

    #[test]
    fn reports_malformed_schema_declarations_with_source_spans() {
        let source = "CREATE SCHEMA crm.;\nCREATE SCHEMA tasks";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.schemas().len(), 0);
        assert_eq!(parsed.diagnostics().len(), 2);
        assert_eq!(parsed.diagnostics()[0].code, "ORNA0001");
        assert_eq!(parsed.diagnostics()[0].span.start, 18);
        assert_eq!(parsed.diagnostics()[1].code, "ORNA0001");
        assert_eq!(parsed.diagnostics()[1].span.start, source.len());
    }

    #[test]
    fn reports_unterminated_comments_without_losing_source() {
        let source = "CREATE SCHEMA crm; /* unfinished";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.diagnostics()[0].code, "ORNA0002");
        assert_eq!(parsed.diagnostics()[0].span.start, 19);
        assert_eq!(parsed.diagnostics()[0].span.end, source.len());
    }

    #[test]
    fn parses_server_functions_with_rows_returns_and_select_bodies() {
        let source = "CREATE SERVER FUNCTION tasks.overdue (\n\
            p_principal REF sys.security.principal DEFAULT sys.security.session_principal(),\n\
            p_before TIMESTAMP DEFAULT tasks.window(sys.time.now(), sys.time.plus(1, 2))\n\
        )\n\
        RETURNS ROWS (\n\
            task REF tasks.task,\n\
            title TEXT\n\
        )\n\
        SECURITY INVOKER\n\
        TRANSACTION READ ONLY\n\
        VOLATILITY STABLE\n\
        AS SELECT REF(t), t.title, t.assignee.name FROM tasks.task t WHERE t.completed = FALSE ORDER BY t.due_at;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.server_functions().len(), 1);

        let function = &parsed.server_functions()[0];
        assert_eq!(function.name.parts[0].text, "tasks");
        assert_eq!(function.name.parts[1].text, "overdue");
        assert_eq!(function.parameters.len(), 2);
        assert_eq!(function.parameters[0].name.text, "p_principal");
        assert_eq!(function.parameters[0].order, 0);
        assert_reference_type(
            &function.parameters[0].type_specification,
            "sys",
            "security",
            "principal",
        );
        assert_eq!(
            function.parameters[0]
                .default_expression
                .as_ref()
                .map(|expression| expression.text.as_str()),
            Some("sys.security.session_principal()"),
        );
        assert_eq!(function.parameters[1].name.text, "p_before");
        assert_eq!(function.parameters[1].order, 1);
        assert_named_type(&function.parameters[1].type_specification, "TIMESTAMP");
        assert_eq!(
            function.parameters[1]
                .default_expression
                .as_ref()
                .map(|expression| expression.text.as_str()),
            Some("tasks.window(sys.time.now(), sys.time.plus(1, 2))"),
        );

        match &function.return_type {
            FunctionReturnType::Rows { columns, .. } => {
                assert_eq!(columns.len(), 2);
                assert_eq!(columns[0].name.text, "task");
                assert_eq!(columns[0].order, 0);
                assert_reference_type(&columns[0].type_specification, "tasks", "task", "");
                assert_eq!(columns[1].name.text, "title");
                assert_eq!(columns[1].order, 1);
                assert_named_type(&columns[1].type_specification, "TEXT");
            }
            FunctionReturnType::Single(_) | FunctionReturnType::Stream { .. } => {
                panic!("tasks.overdue must return rows")
            }
        }
        assert_eq!(function.security, Some(FunctionSecurity::Invoker));
        assert_eq!(function.transaction, Some(FunctionTransaction::ReadOnly));
        assert_eq!(function.volatility, Some(FunctionVolatility::Stable));
        match &function.body {
            ServerFunctionBody::SqlQuery(query) => {
                assert_eq!(
                    query.source.text,
                    "SELECT REF(t), t.title, t.assignee.name FROM tasks.task t WHERE t.completed = FALSE ORDER BY t.due_at",
                );
                assert_eq!(
                    query.source.span.start,
                    source.find("SELECT").expect("query exists")
                );
                assert_eq!(query.query.projections.len(), 3);
                assert!(matches!(
                    query.query.projections[0],
                    QueryExpression::ObjectReference { .. }
                ));
                match &query.query.projections[2] {
                    QueryExpression::FieldPath { root, members, .. } => {
                        assert_eq!(root.text, "t");
                        assert_eq!(members[0].text, "assignee");
                        assert_eq!(members[1].text, "name");
                    }
                    _ => panic!("third projection must be a field path"),
                }
                assert!(matches!(
                    query.query.predicate,
                    Some(QueryExpression::Equality { .. })
                ));
                assert_eq!(query.query.ordering.len(), 1);
                assert_eq!(
                    query.query.ordering[0].direction,
                    OrderingDirection::Unspecified
                );
                assert_eq!(
                    query.query.ordering[0].null_order,
                    NullOrdering::Unspecified
                );
            }
            ServerFunctionBody::SqlInsert(_)
            | ServerFunctionBody::SqlUpdate(_)
            | ServerFunctionBody::SqlDelete(_)
            | ServerFunctionBody::NoInputParameterSelect(_) => {
                panic!("tasks.overdue must use a SELECT body")
            }
        }
    }

    #[test]
    fn parses_distinct_losslessly_with_quoted_source_and_type_neutral_syntax() {
        let source = "CREATE SERVER FUNCTION tasks.values() RETURNS ROWS (value TEXT) \
            AS SELECT DiStInCt \"item\".\"value\" FROM \"tasks\".\"item\" AS \"item\";";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        let ServerFunctionBody::SqlQuery(query) = &parsed.server_functions()[0].body else {
            panic!("DISTINCT source must parse as a SELECT query");
        };
        let SelectQuantifier::Distinct { source: distinct } = &query.query.quantifier else {
            panic!("query must retain DISTINCT instead of the implicit ALL form");
        };
        let distinct_start = source.find("DiStInCt").expect("DISTINCT exists");
        assert_eq!(distinct.text, "DiStInCt");
        assert_eq!(
            distinct.span,
            SourceSpan {
                start: distinct_start,
                end: distinct_start + "DiStInCt".len(),
            }
        );
        assert_eq!(
            query.source.text,
            "SELECT DiStInCt \"item\".\"value\" FROM \"tasks\".\"item\" AS \"item\""
        );
        assert_eq!(
            query.source.span.start,
            source.find("SELECT").expect("SELECT exists")
        );
        assert_eq!(
            query.query.source_object.alias.text, "\"item\"",
            "quoted aliases must remain lossless around DISTINCT"
        );
    }

    #[test]
    fn select_without_distinct_retains_the_implicit_all_quantifier() {
        let source = "CREATE SERVER FUNCTION tasks.values() RETURNS ROWS (value INT) \
            AS SELECT item.value FROM tasks.item item;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        let ServerFunctionBody::SqlQuery(query) = &parsed.server_functions()[0].body else {
            panic!("ordinary SELECT source must parse as a query");
        };
        assert!(matches!(query.query.quantifier, SelectQuantifier::All));
    }

    #[test]
    fn rejects_distinct_order_by_at_order_and_recovers_to_the_next_declaration() {
        let source = "CREATE SERVER FUNCTION tasks.bad() RETURNS ROWS (value INT) \
            AS SELECT DISTINCT item.value FROM tasks.item item ORDER BY item.value; \
            CREATE SCHEMA later;";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.diagnostics().len(), 1);
        let diagnostic = &parsed.diagnostics()[0];
        assert_eq!(diagnostic.code, "ORNA0001");
        assert_eq!(
            diagnostic.message,
            "SELECT DISTINCT queries do not allow ORDER BY; remove the ORDER BY clause",
        );
        let order_start = source.find("ORDER BY").expect("ORDER exists");
        assert_eq!(
            diagnostic.span,
            SourceSpan {
                start: order_start,
                end: order_start + "ORDER".len(),
            }
        );
        assert!(parsed.server_functions().is_empty());
        assert_eq!(parsed.schemas().len(), 1);
        assert_eq!(parsed.schemas()[0].name.parts[0].text, "later");
    }

    #[test]
    fn rejects_deferred_distinct_on_and_select_all_syntax() {
        let distinct_on = "CREATE SERVER FUNCTION tasks.bad() RETURNS ROWS (value INT) \
            AS SELECT DISTINCT ON (item.value) item.value FROM tasks.item item;";
        let parsed = parse(distinct_on);
        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(parsed.diagnostics()[0].code, "ORNA0001");
        assert_eq!(
            parsed.diagnostics()[0].message,
            "DISTINCT ON is not supported; use SELECT DISTINCT followed by the result columns",
        );
        let on_start =
            distinct_on.find("DISTINCT ON").expect("DISTINCT ON exists") + "DISTINCT ".len();
        assert_eq!(
            parsed.diagnostics()[0].span,
            SourceSpan {
                start: on_start,
                end: on_start + "ON".len(),
            }
        );

        let select_all = "CREATE SERVER FUNCTION tasks.bad() RETURNS ROWS (value INT) \
            AS SELECT ALL item.value FROM tasks.item item;";
        let parsed = parse(select_all);
        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(parsed.diagnostics()[0].code, "ORNA0001");
        assert_eq!(
            parsed.diagnostics()[0].message,
            "SELECT ALL is not supported; omit ALL to preserve duplicate rows",
        );
        let all_start = select_all.find("ALL").expect("ALL exists");
        assert_eq!(
            parsed.diagnostics()[0].span,
            SourceSpan {
                start: all_start,
                end: all_start + "ALL".len(),
            }
        );
    }

    #[test]
    fn parses_single_return_types_and_all_server_execution_modifiers() {
        let source = "CREATE SERVER FUNCTION tasks.reopen()\n\
            RETURNS REF tasks.task\n\
            SECURITY DEFINER\n\
            TRANSACTION ATOMIC\n\
            VOLATILITY IMMUTABLE\n\
            AS SELECT REF(t) FROM tasks.task t;\n\
            CREATE SERVER FUNCTION tasks.audit()\n\
            RETURNS TEXT\n\
            TRANSACTION MANUAL\n\
            VOLATILITY VOLATILE\n\
            AS SELECT t.title FROM tasks.task t;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.server_functions().len(), 2);
        match &parsed.server_functions()[0].return_type {
            FunctionReturnType::Single(type_specification) => {
                assert_reference_type(type_specification, "tasks", "task", "");
            }
            FunctionReturnType::Rows { .. } | FunctionReturnType::Stream { .. } => {
                panic!("tasks.reopen must return one reference")
            }
        }
        assert_eq!(
            parsed.server_functions()[0].security,
            Some(FunctionSecurity::Definer),
        );
        assert_eq!(
            parsed.server_functions()[0].transaction,
            Some(FunctionTransaction::Atomic),
        );
        assert_eq!(
            parsed.server_functions()[0].volatility,
            Some(FunctionVolatility::Immutable),
        );
        assert_eq!(parsed.server_functions()[1].security, None);
        assert_eq!(
            parsed.server_functions()[1].transaction,
            Some(FunctionTransaction::Manual),
        );
        assert_eq!(
            parsed.server_functions()[1].volatility,
            Some(FunctionVolatility::Volatile),
        );
    }

    #[test]
    fn parses_server_function_capabilities_after_execution_modifiers() {
        let source = "CREATE SERVER FUNCTION security.rotate_key(p_key TEXT)\n\
            RETURNS BOOL\n\
            SECURITY DEFINER\n\
            TRANSACTION ATOMIC\n\
            VOLATILITY VOLATILE\n\
            REQUIRES CAPABILITY sys.secret.read(p_key, audit(sys.time.now(), p_actor)),\n\
                std.net.call(\n\
                    endpoint => p_endpoint,\n\
                    metadata => trace(request(1, 2))\n\
                ),\n\
                sys.job.submit,\n\
                sys.job.noop()\n\
            AS SELECT t.completed FROM tasks.task t;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);

        let capabilities = &parsed.server_functions()[0].capabilities;
        assert_eq!(capabilities.len(), 4);
        assert_eq!(capabilities[0].name.parts[0].text, "sys");
        assert_eq!(capabilities[0].name.parts[1].text, "secret");
        assert_eq!(capabilities[0].name.parts[2].text, "read");
        assert_eq!(
            capabilities[0]
                .arguments
                .as_ref()
                .map(|arguments| arguments.text.as_str()),
            Some("p_key, audit(sys.time.now(), p_actor)"),
        );
        assert_eq!(
            capabilities[1]
                .arguments
                .as_ref()
                .map(|arguments| arguments.text.as_str()),
            Some("\nendpoint => p_endpoint,\nmetadata => trace(request(1, 2))\n"),
        );
        assert!(capabilities[2].arguments.is_none());
        assert_eq!(
            capabilities[3]
                .arguments
                .as_ref()
                .map(|arguments| arguments.text.as_str()),
            Some(""),
        );
        assert_eq!(
            capabilities[1]
                .arguments
                .as_ref()
                .expect("arguments exist")
                .span
                .start,
            source.find("\nendpoint").expect("arguments exist"),
        );
    }

    #[test]
    fn rejects_malformed_server_function_capability_clauses() {
        let sources = [
            (
                "CREATE SERVER FUNCTION security.bad() RETURNS BOOL REQUIRES CAPABILITY AS SELECT TRUE;",
                "expected a capability",
            ),
            (
                "CREATE SERVER FUNCTION security.bad() RETURNS BOOL REQUIRES CAPABILITY sys.secret.read(), AS SELECT TRUE;",
                "trailing commas",
            ),
            (
                "CREATE SERVER FUNCTION security.bad() RETURNS BOOL REQUIRES CAPABILITY sys.secret.read(p_key AS SELECT TRUE;",
                "expected ')'",
            ),
        ];

        for (source, expected_message) in sources {
            let parsed = parse(source);

            assert_eq!(parsed.syntax().text(), source);
            assert!(parsed.server_functions().is_empty());
            assert!(
                parsed
                    .diagnostics()
                    .iter()
                    .any(|diagnostic| diagnostic.code == "ORNA0001"),
            );
            assert!(
                parsed
                    .diagnostics()
                    .iter()
                    .any(|diagnostic| diagnostic.message.contains(expected_message)),
            );
        }
    }

    #[test]
    fn parses_canonical_multiword_scalar_types_in_every_type_position() {
        let source = "CREATE TYPE files.document AS OBJECT (body CHARACTER LARGE OBJECT, content BINARY LARGE OBJECT);\n\
            CREATE SERVER FUNCTION files.encode(input CHARACTER LARGE OBJECT)\n\
            RETURNS BINARY LARGE OBJECT\n\
            AS SELECT REF(d) FROM files.document d;\n\
            CREATE SERVER FUNCTION files.describe()\n\
            RETURNS ROWS (body CHARACTER LARGE OBJECT, content BINARY LARGE OBJECT)\n\
            AS SELECT REF(d) FROM files.document d;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);

        let fields = &parsed.object_types()[0].fields;
        assert_standard_large_object_type(
            &fields[0].type_specification,
            StandardLargeObjectKind::Character,
            "CHARACTER LARGE OBJECT",
        );
        assert_standard_large_object_type(
            &fields[1].type_specification,
            StandardLargeObjectKind::Binary,
            "BINARY LARGE OBJECT",
        );

        let encode = &parsed.server_functions()[0];
        assert_standard_large_object_type(
            &encode.parameters[0].type_specification,
            StandardLargeObjectKind::Character,
            "CHARACTER LARGE OBJECT",
        );
        match &encode.return_type {
            FunctionReturnType::Single(type_specification) => {
                assert_standard_large_object_type(
                    type_specification,
                    StandardLargeObjectKind::Binary,
                    "BINARY LARGE OBJECT",
                );
            }
            FunctionReturnType::Rows { .. } | FunctionReturnType::Stream { .. } => {
                panic!("files.encode must return one value")
            }
        }

        let describe = &parsed.server_functions()[1];
        match &describe.return_type {
            FunctionReturnType::Rows { columns, .. } => {
                assert_standard_large_object_type(
                    &columns[0].type_specification,
                    StandardLargeObjectKind::Character,
                    "CHARACTER LARGE OBJECT",
                );
                assert_standard_large_object_type(
                    &columns[1].type_specification,
                    StandardLargeObjectKind::Binary,
                    "BINARY LARGE OBJECT",
                );
            }
            FunctionReturnType::Single(_) | FunctionReturnType::Stream { .. } => {
                panic!("files.describe must return rows")
            }
        }
    }

    #[test]
    fn retains_exact_source_for_multiword_large_object_types() {
        let source =
            "CREATE TYPE files.document AS OBJECT (body cHaRaCtEr /* kept */ LaRgE ObJeCt);";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);

        match &parsed.object_types()[0].fields[0].type_specification {
            TypeSpecification::StandardLargeObject { kind, source } => {
                assert_eq!(*kind, StandardLargeObjectKind::Character);
                assert_eq!(source.text, "cHaRaCtEr /* kept */ LaRgE ObJeCt");
            }
            _ => {
                panic!("body must use the standard large object AST form")
            }
        }
    }

    #[test]
    fn parses_constructed_type_specifications_losslessly() {
        let source = "CREATE TYPE samples.container AS OBJECT (\
            listed LIST /* kept */ < TEXT >,\
            unique SET<REF tasks.task>,\
            indexed MAP<TEXT, OPTION<BOOL>>,\
            optional TEXT /* first */ ? /* second */ ?,\
            streamed STREAM<tasks.event>,\
            recursive REF LIST<TEXT>\
        );";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        let fields = &parsed.object_types()[0].fields;

        let TypeSpecification::List { element, span } = &fields[0].type_specification else {
            panic!("listed must use LIST");
        };
        assert_eq!(&source[span.start..span.end], "LIST /* kept */ < TEXT >");
        assert_named_type(element, "TEXT");

        let TypeSpecification::Set { element, .. } = &fields[1].type_specification else {
            panic!("unique must use SET");
        };
        let TypeSpecification::Reference { target, .. } = element.as_ref() else {
            panic!("SET element must use REF");
        };
        assert_named_type(target, "tasks.task");

        let TypeSpecification::Map { key, value, .. } = &fields[2].type_specification else {
            panic!("indexed must use MAP");
        };
        assert_named_type(key, "TEXT");
        let TypeSpecification::Option {
            value,
            spelling: OptionTypeSpelling::Prefix,
            ..
        } = value.as_ref()
        else {
            panic!("MAP value must use prefix OPTION");
        };
        assert_named_type(value, "BOOL");

        let TypeSpecification::Option {
            value,
            spelling: OptionTypeSpelling::Postfix,
            span,
        } = &fields[3].type_specification
        else {
            panic!("optional must use postfix OPTION");
        };
        assert_eq!(
            &source[span.start..span.end],
            "TEXT /* first */ ? /* second */ ?"
        );
        let TypeSpecification::Option {
            value,
            spelling: OptionTypeSpelling::Postfix,
            ..
        } = value.as_ref()
        else {
            panic!("optional must retain both postfix markers");
        };
        assert_named_type(value, "TEXT");

        let TypeSpecification::Stream { element, .. } = &fields[4].type_specification else {
            panic!("streamed must use STREAM");
        };
        assert_named_type(element, "tasks.event");

        let TypeSpecification::Reference { target, .. } = &fields[5].type_specification else {
            panic!("recursive must use REF");
        };
        assert!(matches!(target.as_ref(), TypeSpecification::List { .. }));
    }

    #[test]
    fn constructed_type_errors_recover_to_a_later_declaration() {
        let malformed = "CREATE TYPE samples.bad AS OBJECT (value MAP<TEXT OPTION<BOOL>>);\
            CREATE SCHEMA recovered;";
        let parsed = parse(malformed);

        assert_eq!(parsed.syntax().text(), malformed);
        assert!(parsed.object_types().is_empty());
        assert_eq!(parsed.schemas().len(), 1);
        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(
            parsed.diagnostics()[0].message,
            "expected ',' between MAP key and value types"
        );

        let nested = format!(
            "CREATE TYPE samples.deep AS OBJECT (value {}TEXT{});CREATE SCHEMA after_depth;",
            "OPTION<".repeat(33),
            ">".repeat(33)
        );
        let parsed = parse(&nested);
        assert_eq!(parsed.syntax().text(), nested);
        assert!(parsed.object_types().is_empty());
        assert_eq!(parsed.schemas().len(), 1);
        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(
            parsed.diagnostics()[0].message,
            "type specification exceeds the maximum depth of 32"
        );

        let mixed = format!(
            "CREATE TYPE samples.deep AS OBJECT (value LIST<TEXT{}>);CREATE SCHEMA after_mixed;",
            "?".repeat(32)
        );
        let parsed = parse(&mixed);
        assert_eq!(parsed.syntax().text(), mixed);
        assert!(parsed.object_types().is_empty());
        assert_eq!(parsed.schemas().len(), 1);
        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(
            parsed.diagnostics()[0].message,
            "type specification exceeds the maximum depth of 32"
        );
    }

    #[test]
    fn every_constructed_type_delimiter_failure_is_direct_and_recoverable() {
        for (written_type, expected) in [
            ("LIST TEXT", "expected '<' after type constructor"),
            ("LIST<>", "expected a field type"),
            ("LIST<TEXT", "expected '>' to close type constructor"),
            ("MAP<, TEXT>", "expected a field type"),
            ("MAP<TEXT, >", "expected a field type"),
            (
                "MAP<TEXT TEXT>",
                "expected ',' between MAP key and value types",
            ),
        ] {
            let source = format!(
                "CREATE TYPE samples.bad AS OBJECT (value {written_type});CREATE SCHEMA recovered;"
            );
            let parsed = parse(&source);

            assert_eq!(parsed.syntax().text(), source, "{written_type}");
            assert!(parsed.object_types().is_empty(), "{written_type}");
            assert_eq!(parsed.schemas().len(), 1, "{written_type}");
            assert_eq!(
                parsed.diagnostics().len(),
                1,
                "{written_type}: {:?}",
                parsed.diagnostics()
            );
            assert_eq!(parsed.diagnostics()[0].message, expected, "{written_type}");
        }
    }

    #[test]
    fn parses_stream_return_type_losslessly_with_complete_span() {
        let source = "CREATE SERVER FUNCTION tasks.events() RETURNS STREAM< /* kept */ REF tasks.event > AS SELECT REF(e) FROM tasks.event e;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        let FunctionReturnType::Stream { element, span } =
            &parsed.server_functions()[0].return_type
        else {
            panic!("events must return a stream");
        };
        assert_eq!(
            &source[span.start..span.end],
            "STREAM< /* kept */ REF tasks.event >"
        );
        assert_reference_type(element, "tasks", "event", "");
    }

    #[test]
    fn malformed_stream_return_types_keep_source_and_recover() {
        for (written_type, expected) in [
            ("STREAM", "expected '<' after type constructor"),
            ("STREAM<>", "expected a field type"),
            ("STREAM<TEXT", "expected '>' to close type constructor"),
        ] {
            let source = format!(
                "CREATE SERVER FUNCTION tasks.bad() RETURNS {written_type} AS SELECT TRUE;CREATE SCHEMA recovered;"
            );
            let parsed = parse(&source);

            assert_eq!(parsed.syntax().text(), source, "{written_type}");
            assert!(parsed.server_functions().is_empty(), "{written_type}");
            assert_eq!(parsed.schemas().len(), 1, "{written_type}");
            assert_eq!(
                parsed.diagnostics().len(),
                1,
                "{written_type}: {:?}",
                parsed.diagnostics()
            );
            assert_eq!(parsed.diagnostics()[0].message, expected, "{written_type}");
        }
    }

    #[test]
    fn rejects_legacy_table_and_set_of_return_declarations() {
        let source = "CREATE SERVER FUNCTION tasks.table_result() RETURNS TABLE (task REF tasks.task) AS SELECT REF(t) FROM tasks.task t;\n\
            CREATE SERVER FUNCTION tasks.set_result() RETURNS SET OF REF tasks.task AS SELECT REF(t) FROM tasks.task t;";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert!(parsed.server_functions().is_empty());
        assert_eq!(parsed.diagnostics().len(), 2);
        assert!(
            parsed
                .diagnostics()
                .iter()
                .all(|diagnostic| diagnostic.code == "ORNA0001")
        );
        assert!(parsed.diagnostics()[0].message.contains("RETURNS TABLE"));
        assert!(parsed.diagnostics()[0].message.contains("RETURNS ROWS"));
        assert!(parsed.diagnostics()[1].message.contains("RETURNS SET OF"));
        assert!(parsed.diagnostics()[1].message.contains("RETURNS ROWS"));
    }

    #[test]
    fn rejects_proposal_only_declarations_without_losing_following_schema() {
        let cases = [
            "CREATE APPLICATION app; CREATE SCHEMA recovered;",
            "CREATE COMPONENT app.widget; CREATE SCHEMA recovered;",
            "CREATE QUERY app.list; CREATE SCHEMA recovered;",
            "CREATE SCREEN app.home; CREATE SCHEMA recovered;",
            "CREATE PAGE app.home; CREATE SCHEMA recovered;",
            "CREATE SERVER FUNCTION tasks.list() RETURNS ROWS (task REF tasks.task) AS SELECT REF(t) FROM tasks.task t RUNS ON SERVER; CREATE SCHEMA recovered;",
        ];

        for source in cases {
            let parsed = parse(source);

            assert_eq!(parsed.syntax().text(), source, "{source}");
            assert_eq!(parsed.diagnostics().len(), 1, "{source}");
            assert_eq!(parsed.diagnostics()[0].code, "ORNA0001", "{source}");
            assert!(parsed.object_types().is_empty(), "{source}");
            assert!(parsed.enum_types().is_empty(), "{source}");
            assert!(parsed.record_value_types().is_empty(), "{source}");
            assert!(parsed.primitive_value_types().is_empty(), "{source}");
            assert!(parsed.opaque_value_types().is_empty(), "{source}");
            assert!(parsed.type_exports().is_empty(), "{source}");
            assert!(parsed.field_renames().is_empty(), "{source}");
            assert!(parsed.server_functions().is_empty(), "{source}");
            assert!(parsed.client_functions().is_empty(), "{source}");
            assert_eq!(parsed.schemas().len(), 1, "{source}");
            assert_eq!(
                parsed.schemas()[0].name.parts[0].text,
                "recovered",
                "{source}"
            );
        }
    }

    #[test]
    fn rejects_nonstandard_trailing_commas_in_server_function_shapes() {
        let parameters = "CREATE SERVER FUNCTION tasks.bad(p_task REF tasks.task,) RETURNS TEXT AS SELECT 'bad';";
        let rows = "CREATE SERVER FUNCTION tasks.bad() RETURNS ROWS (task REF tasks.task,) AS SELECT REF(t) FROM tasks.task t;";

        for source in [parameters, rows] {
            let parsed = parse(source);

            assert_eq!(parsed.syntax().text(), source);
            assert!(parsed.server_functions().is_empty());
            assert_eq!(parsed.diagnostics().len(), 1);
            assert_eq!(parsed.diagnostics()[0].code, "ORNA0001");
            assert!(parsed.diagnostics()[0].message.contains("trailing commas"));
        }
    }

    #[test]
    fn preserves_select_source_spans_and_trivia() {
        let source = "CREATE SERVER FUNCTION tasks.list() RETURNS ROWS (task REF tasks.task, title TEXT) AS\n\
            SELECT /* identity */ REF( t ), t /* title root */ . title, t.assignee /* member */ . name\n\
            FROM tasks /* object namespace */ . task AS t\n\
            WHERE t.completed /* equality */ = fAlSe\n\
            ORDER BY t.due_at DESC, t.title ASC;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        let body = parsed.server_functions()[0]
            .body
            .as_sql_query()
            .expect("tasks.list must use a SELECT body");
        let query_start = source.find("SELECT").expect("query exists");
        let query_end = source.rfind("ASC").expect("ordering direction exists") + "ASC".len();
        assert_eq!(&body.source.text, &source[query_start..query_end]);
        assert_eq!(body.source.span.start, query_start);
        assert_eq!(body.source.span.end, query_end);
        assert_eq!(body.query.span.start, query_start);
        assert_eq!(body.query.span.end, query_end);
        assert_eq!(body.query.source_object.object_type.parts[0].text, "tasks");
        assert_eq!(body.query.source_object.object_type.parts[1].text, "task");
        assert_eq!(body.query.source_object.alias.text, "t");
        assert_eq!(
            body.query.ordering[0].direction,
            OrderingDirection::Descending
        );
        assert_eq!(body.query.ordering[0].null_order, NullOrdering::Unspecified);
        assert_eq!(
            body.query.ordering[1].direction,
            OrderingDirection::Ascending
        );
        assert_eq!(body.query.ordering[1].null_order, NullOrdering::Unspecified);

        match &body.query.predicate {
            Some(QueryExpression::Equality { left, right, .. }) => {
                assert_eq!(&source[left.span().start..left.span().end], "t.completed");
                match right.as_ref() {
                    QueryExpression::BooleanLiteral { value, source } => {
                        assert!(!value);
                        assert_eq!(source.text, "fAlSe");
                    }
                    _ => panic!("right equality expression must be a boolean literal"),
                }
            }
            _ => panic!("query must contain its equality predicate"),
        }
    }

    #[test]
    fn retains_identity_selector_parameters_for_both_source_alias_forms() {
        for (source, selector) in [
            (
                "CREATE SERVER FUNCTION tasks.get(p_task REF tasks.task) RETURNS ROWS (task REF tasks.task) AS SELECT REF(selected) FROM tasks.task selected WHERE REF(selected) = p_task;",
                "p_task",
            ),
            (
                "CREATE SERVER FUNCTION tasks.get(\"p_Task\" REF tasks.task) RETURNS ROWS (task REF tasks.task) AS SELECT REF(selected) FROM tasks.task AS selected WHERE REF(selected) = \"p_Task\";",
                "\"p_Task\"",
            ),
        ] {
            let parsed = parse(source);

            assert!(parsed.diagnostics().is_empty(), "source: {source}");
            assert_eq!(parsed.syntax().text(), source);
            let body = parsed.server_functions()[0]
                .body
                .as_sql_query()
                .expect("function must retain its SELECT query");
            let parameter_start = source.rfind(selector).expect("selector parameter exists");
            let query_start = source.find("SELECT").expect("query exists");
            assert_eq!(body.source.text, &source[query_start..source.len() - 1]);
            assert_eq!(body.query.span.end, parameter_start + selector.len());

            match &body.query.predicate {
                Some(QueryExpression::Equality { left, right, span }) => {
                    assert_eq!(&source[left.span().start..left.span().end], "REF(selected)");
                    match right.as_ref() {
                        QueryExpression::ParameterRead { parameter } => {
                            assert_eq!(parameter.text, selector);
                            assert_eq!(parameter.span.start, parameter_start);
                            assert_eq!(parameter.span.end, parameter_start + selector.len());
                            assert_eq!(&source[parameter.span.start..parameter.span.end], selector);
                        }
                        _ => panic!("selector right operand must retain the parameter read"),
                    }
                    assert_eq!(span.start, left.span().start);
                    assert_eq!(span.end, parameter_start + selector.len());
                }
                _ => panic!("query must contain the identity selector predicate"),
            }
        }
    }

    #[test]
    fn retains_a_parameter_read_after_one_direct_field_path() {
        let source = "CREATE SERVER FUNCTION people.by_email(p_email TEXT) RETURNS ROWS (person REF people.person, name TEXT) AS SELECT REF(selected), selected.name FROM people.person selected WHERE selected.email = p_email;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        let body = parsed.server_functions()[0]
            .body
            .as_sql_query()
            .expect("people.by_email must use a SELECT query");
        let parameter_start = source.rfind("p_email").expect("parameter exists");
        match &body.query.predicate {
            Some(QueryExpression::Equality { left, right, span }) => {
                assert!(matches!(
                    left.as_ref(),
                    QueryExpression::FieldPath { root, members, .. }
                        if root.text == "selected" && members.len() == 1 && members[0].text == "email"
                ));
                match right.as_ref() {
                    QueryExpression::ParameterRead { parameter } => {
                        assert_eq!(parameter.text, "p_email");
                        assert_eq!(parameter.span.start, parameter_start);
                        assert_eq!(parameter.span.end, parameter_start + "p_email".len());
                    }
                    _ => panic!("direct field selector must retain a parameter read"),
                }
                assert_eq!(span.start, left.span().start);
                assert_eq!(span.end, parameter_start + "p_email".len());
            }
            _ => panic!("query must contain a direct-field selector predicate"),
        }
    }

    #[test]
    fn rejects_a_bare_selector_name_after_a_nested_field_path() {
        let source = "CREATE SERVER FUNCTION people.by_nested_email(p_email TEXT) RETURNS ROWS (person REF people.person) AS SELECT REF(selected) FROM people.person selected WHERE selected.owner.email = p_email;";
        let parsed = parse(source);

        assert!(parsed.server_functions().is_empty());
        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(parsed.diagnostics()[0].code, "ORNA0001");
        assert_eq!(
            parsed.diagnostics()[0].message,
            "the current Orna SELECT parser does not yet implement bare alias expressions; expected a field path"
        );
    }

    #[test]
    fn retains_direct_field_selector_parser_closures() {
        let reversed = "CREATE SERVER FUNCTION people.reversed(p_email TEXT) RETURNS ROWS (person REF people.person) AS SELECT REF(selected) FROM people.person selected WHERE p_email = selected.email;";
        let parsed = parse(reversed);

        assert!(parsed.server_functions().is_empty());
        assert_eq!(parsed.diagnostics().len(), 1);
        let diagnostic = &parsed.diagnostics()[0];
        assert_eq!(diagnostic.code, "ORNA0001");
        assert_eq!(
            diagnostic.message,
            "the current Orna SELECT parser does not yet implement bare alias expressions; expected a field path",
            "{diagnostic:#?}"
        );
        let reversed_start = reversed.find(" = selected").expect("equality exists") + 1;
        assert_eq!(
            diagnostic.span,
            SourceSpan {
                start: reversed_start,
                end: reversed_start + "=".len(),
            }
        );

        let qualified = "CREATE SERVER FUNCTION people.qualified(p_email TEXT) RETURNS ROWS (person REF people.person) AS SELECT REF(selected) FROM people.person selected WHERE selected.email = owner.p_email;";
        let parsed = parse(qualified);

        assert!(parsed.diagnostics().is_empty());
        let body = parsed.server_functions()[0]
            .body
            .as_sql_query()
            .expect("people.qualified must use a SELECT query");
        match &body.query.predicate {
            Some(QueryExpression::Equality { right, .. }) => assert!(matches!(
                right.as_ref(),
                QueryExpression::FieldPath { root, members, .. }
                    if root.text == "owner" && members.len() == 1 && members[0].text == "p_email"
            )),
            _ => panic!("query must retain its qualified right-hand path"),
        }

        let call = "CREATE SERVER FUNCTION people.call(p_email TEXT) RETURNS ROWS (person REF people.person) AS SELECT REF(selected) FROM people.person selected WHERE selected.email = find_email();";
        let parsed = parse(call);

        assert!(parsed.server_functions().is_empty());
        assert_eq!(parsed.diagnostics().len(), 1);
        let diagnostic = &parsed.diagnostics()[0];
        assert_eq!(diagnostic.code, "ORNA0001");
        assert_eq!(
            diagnostic.message,
            "the current Orna SELECT parser does not yet implement function calls as identity selector parameters; expected a selector parameter name by itself"
        );
        let call_start =
            call.find("find_email()").expect("function call exists") + "find_email".len();
        assert_eq!(
            diagnostic.span,
            SourceSpan {
                start: call_start,
                end: call_start + "(".len(),
            }
        );
    }

    #[test]
    fn preserves_existing_ref_and_boolean_right_operands_after_object_references() {
        let cases = [
            (
                "CREATE SERVER FUNCTION tasks.ref_equal() RETURNS ROWS (task REF tasks.task) AS SELECT REF(t) FROM tasks.task t WHERE REF(t) = REF(t);",
                "REF(t)",
            ),
            (
                "CREATE SERVER FUNCTION tasks.ref_true() RETURNS ROWS (task REF tasks.task) AS SELECT REF(t) FROM tasks.task t WHERE REF(t) = TRUE;",
                "TRUE",
            ),
        ];

        for (source, right_source) in cases {
            let parsed = parse(source);

            assert!(parsed.diagnostics().is_empty(), "source: {source}");
            let body = parsed.server_functions()[0]
                .body
                .as_sql_query()
                .expect("function must retain its SELECT query");
            match &body.query.predicate {
                Some(QueryExpression::Equality { right, .. }) => {
                    assert_eq!(&source[right.span().start..right.span().end], right_source,);
                    if right_source == "REF(t)" {
                        assert!(matches!(
                            right.as_ref(),
                            QueryExpression::ObjectReference { alias, .. } if alias.text == "t"
                        ));
                    } else {
                        assert!(matches!(
                            right.as_ref(),
                            QueryExpression::BooleanLiteral { value: true, .. }
                        ));
                    }
                }
                _ => panic!("query must retain its equality predicate"),
            }
        }
    }

    #[test]
    fn preserves_existing_boolean_left_equality_with_an_object_reference() {
        let source = "CREATE SERVER FUNCTION tasks.true_equal() RETURNS ROWS (task REF tasks.task) AS SELECT REF(t) FROM tasks.task t WHERE TRUE = REF(t);";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        let body = parsed.server_functions()[0]
            .body
            .as_sql_query()
            .expect("function must retain its SELECT query");
        match &body.query.predicate {
            Some(QueryExpression::Equality { left, right, .. }) => {
                assert!(matches!(
                    left.as_ref(),
                    QueryExpression::BooleanLiteral { value: true, .. }
                ));
                assert!(matches!(
                    right.as_ref(),
                    QueryExpression::ObjectReference { alias, .. } if alias.text == "t"
                ));
            }
            _ => panic!("query must retain its equality predicate"),
        }
    }

    #[test]
    fn retains_direct_boolean_where_predicates_for_implicit_all_losslessly() {
        let source = "CREATE SERVER FUNCTION tasks.by_field() RETURNS ROWS (completed BOOL) AS SELECT t.completed FROM tasks.task t WHERE t.completed;\n\
            CREATE SERVER FUNCTION tasks.by_true() RETURNS ROWS (completed BOOL) AS SELECT t.completed FROM tasks.task t WHERE TRUE;\n\
            CREATE SERVER FUNCTION tasks.by_false() RETURNS ROWS (completed BOOL) AS SELECT t.completed FROM tasks.task t WHERE fAlSe;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.server_functions().len(), 3);

        let field = parsed.server_functions()[0]
            .body
            .as_sql_query()
            .expect("field predicate function must use a SELECT body");
        assert!(matches!(field.query.quantifier, SelectQuantifier::All));
        let field_start = source
            .find("WHERE t.completed")
            .expect("field predicate exists")
            + "WHERE ".len();
        match field.query.predicate.as_ref() {
            Some(QueryExpression::FieldPath {
                root,
                members,
                span,
            }) => {
                assert_eq!(root.text, "t");
                assert_eq!(members.len(), 1);
                assert_eq!(members[0].text, "completed");
                assert_eq!(
                    span,
                    &SourceSpan {
                        start: field_start,
                        end: field_start + "t.completed".len(),
                    }
                );
                assert_eq!(&source[span.start..span.end], "t.completed");
            }
            _ => panic!("WHERE t.completed must remain a field predicate"),
        }

        for (function, source_text, value) in [(1, "TRUE", true), (2, "fAlSe", false)] {
            let query = parsed.server_functions()[function]
                .body
                .as_sql_query()
                .expect("boolean predicate function must use a SELECT body");
            assert!(matches!(query.query.quantifier, SelectQuantifier::All));
            let literal_start = source
                .find(&format!("WHERE {source_text}"))
                .expect("literal predicate exists")
                + "WHERE ".len();
            match query.query.predicate.as_ref() {
                Some(QueryExpression::BooleanLiteral {
                    value: actual_value,
                    source: literal,
                }) => {
                    assert_eq!(*actual_value, value);
                    assert_eq!(literal.text, source_text);
                    assert_eq!(
                        literal.span,
                        SourceSpan {
                            start: literal_start,
                            end: literal_start + source_text.len(),
                        }
                    );
                    assert_eq!(&source[literal.span.start..literal.span.end], source_text,);
                }
                _ => panic!("WHERE {source_text} must remain a boolean predicate"),
            }
        }
    }

    #[test]
    fn rejects_direct_ref_where_predicates_at_the_complete_predicate_span_and_recovers() {
        let source = "CREATE SERVER FUNCTION tasks.bad() RETURNS ROWS (task REF tasks.task) AS SELECT REF(t) FROM tasks.task t WHERE REF(t);\n\
            CREATE SERVER FUNCTION tasks.good() RETURNS ROWS (task REF tasks.task) AS SELECT REF(t) FROM tasks.task t WHERE TRUE;";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.diagnostics().len(), 1);
        let diagnostic = &parsed.diagnostics()[0];
        assert_eq!(diagnostic.code, "ORNA0001");
        assert_eq!(
            diagnostic.message,
            "WHERE must use a BOOLEAN field, TRUE, FALSE, or an equality predicate",
        );
        let predicate_start =
            source.find("WHERE REF(t)").expect("predicate REF exists") + "WHERE ".len();
        assert_eq!(
            diagnostic.span,
            SourceSpan {
                start: predicate_start,
                end: predicate_start + "REF(t)".len(),
            }
        );
        assert_eq!(parsed.server_functions().len(), 1);
        assert_eq!(parsed.server_functions()[0].name.parts[1].text, "good");
    }

    #[test]
    fn retains_direct_boolean_where_predicates_under_distinct_losslessly() {
        let source = "CREATE SERVER FUNCTION tasks.by_field() RETURNS ROWS (value INT) AS SELECT DISTINCT t.value FROM tasks.task t WHERE t.title;\n\
            CREATE SERVER FUNCTION tasks.by_true() RETURNS ROWS (value INT) AS SELECT DISTINCT t.value FROM tasks.task t WHERE TRUE;\n\
            CREATE SERVER FUNCTION tasks.by_false() RETURNS ROWS (value INT) AS SELECT DISTINCT t.value FROM tasks.task t WHERE fAlSe;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.server_functions().len(), 3);

        let field = parsed.server_functions()[0]
            .body
            .as_sql_query()
            .expect("field predicate function must use a SELECT body");
        let distinct_start = source.find("DISTINCT").expect("DISTINCT exists");
        assert!(matches!(
            &field.query.quantifier,
            SelectQuantifier::Distinct { source: distinct }
                if distinct.text == "DISTINCT"
                    && distinct.span == SourceSpan {
                        start: distinct_start,
                        end: distinct_start + "DISTINCT".len(),
                    }
        ));
        let field_start = source
            .find("WHERE t.title")
            .expect("field predicate exists")
            + "WHERE ".len();
        match field.query.predicate.as_ref() {
            Some(QueryExpression::FieldPath {
                root,
                members,
                span,
            }) => {
                assert_eq!(root.text, "t");
                assert_eq!(members.len(), 1);
                assert_eq!(members[0].text, "title");
                assert_eq!(
                    span,
                    &SourceSpan {
                        start: field_start,
                        end: field_start + "t.title".len(),
                    }
                );
                assert_eq!(&source[span.start..span.end], "t.title");
            }
            _ => panic!("WHERE t.title must remain a type-neutral field predicate"),
        }

        for (function, source_text, value) in [(1, "TRUE", true), (2, "fAlSe", false)] {
            let query = parsed.server_functions()[function]
                .body
                .as_sql_query()
                .expect("boolean predicate function must use a SELECT body");
            assert!(matches!(
                query.query.quantifier,
                SelectQuantifier::Distinct { .. }
            ));
            let literal_start = source
                .find(&format!("WHERE {source_text}"))
                .expect("literal predicate exists")
                + "WHERE ".len();
            match query.query.predicate.as_ref() {
                Some(QueryExpression::BooleanLiteral {
                    value: actual_value,
                    source: literal,
                }) => {
                    assert_eq!(*actual_value, value);
                    assert_eq!(literal.text, source_text);
                    assert_eq!(
                        literal.span,
                        SourceSpan {
                            start: literal_start,
                            end: literal_start + source_text.len(),
                        }
                    );
                    assert_eq!(&source[literal.span.start..literal.span.end], source_text,);
                }
                _ => panic!("WHERE {source_text} must remain a Boolean predicate"),
            }
        }
    }

    #[test]
    fn rejects_direct_ref_where_predicates_under_distinct_and_recovers() {
        let source = "CREATE SERVER FUNCTION tasks.bad() RETURNS ROWS (task REF tasks.task) AS SELECT DISTINCT REF(t) FROM tasks.task t WHERE REF(t);\n\
            CREATE SERVER FUNCTION tasks.good_field() RETURNS ROWS (value INT) AS SELECT DISTINCT t.value FROM tasks.task t WHERE t.title;\n\
            CREATE SERVER FUNCTION tasks.good_true() RETURNS ROWS (value INT) AS SELECT DISTINCT t.value FROM tasks.task t WHERE TRUE;\n\
            CREATE SERVER FUNCTION tasks.good_false() RETURNS ROWS (value INT) AS SELECT DISTINCT t.value FROM tasks.task t WHERE FALSE;";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.diagnostics().len(), 1);
        let diagnostic = &parsed.diagnostics()[0];
        assert_eq!(diagnostic.code, "ORNA0001");
        assert_eq!(
            diagnostic.message,
            "WHERE must use a BOOLEAN field, TRUE, FALSE, or an equality predicate",
        );
        let predicate_start =
            source.find("WHERE REF(t)").expect("predicate REF exists") + "WHERE ".len();
        assert_eq!(
            diagnostic.span,
            SourceSpan {
                start: predicate_start,
                end: predicate_start + "REF(t)".len(),
            }
        );
        assert_eq!(parsed.server_functions().len(), 3);
        for (function, name, expected) in [
            (0, "good_field", "field"),
            (1, "good_true", "true"),
            (2, "good_false", "false"),
        ] {
            let declaration = &parsed.server_functions()[function];
            assert_eq!(declaration.name.parts[1].text, name);
            let query = declaration
                .body
                .as_sql_query()
                .expect("recovered function must use a SELECT body");
            assert!(matches!(
                query.query.quantifier,
                SelectQuantifier::Distinct { .. }
            ));
            match (expected, query.query.predicate.as_ref()) {
                ("field", Some(QueryExpression::FieldPath { .. }))
                | ("true", Some(QueryExpression::BooleanLiteral { value: true, .. }))
                | ("false", Some(QueryExpression::BooleanLiteral { value: false, .. })) => {}
                _ => panic!("recovered {name} predicate has the wrong shape"),
            }
        }
    }

    #[test]
    fn rejects_reversed_identity_selector_operands_with_an_exact_diagnostic() {
        let source = "CREATE SERVER FUNCTION tasks.bad(p_task REF tasks.task) RETURNS ROWS (task REF tasks.task) AS SELECT REF(selected) FROM tasks.task selected WHERE p_task = REF(selected);";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert!(parsed.server_functions().is_empty());
        assert_eq!(parsed.diagnostics().len(), 1);
        let diagnostic = &parsed.diagnostics()[0];
        assert_eq!(diagnostic.code, "ORNA0001");
        assert_eq!(
            diagnostic.message,
            "the current Orna SELECT parser does not yet implement selector parameters on the left side of WHERE equality; expected WHERE REF(alias) = selector_parameter",
        );
        let parameter_start =
            source.find("WHERE p_task").expect("selector exists") + "WHERE ".len();
        assert_eq!(
            diagnostic.span,
            SourceSpan {
                start: parameter_start,
                end: parameter_start + "p_task".len(),
            }
        );
    }

    #[test]
    fn keeps_the_existing_bare_projection_diagnostic() {
        let source = "CREATE SERVER FUNCTION tasks.bad(p_task REF tasks.task) RETURNS ROWS (task REF tasks.task) AS SELECT p_task FROM tasks.task selected;";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert!(parsed.server_functions().is_empty());
        assert_eq!(parsed.diagnostics().len(), 1);
        let diagnostic = &parsed.diagnostics()[0];
        assert_eq!(diagnostic.code, "ORNA0001");
        assert_eq!(
            diagnostic.message,
            "the current Orna SELECT parser does not yet implement bare alias expressions; expected a field path",
        );
        let from_start = source.find(" FROM").expect("FROM exists") + 1;
        assert_eq!(
            diagnostic.span,
            SourceSpan {
                start: from_start,
                end: from_start + "FROM".len(),
            }
        );
    }

    #[test]
    fn parses_a_no_input_parameter_select_server_body() {
        let source = "CREATE SERVER FUNCTION f(p_value INTEGER) RETURNS INTEGER AS SELECT p_value;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.server_functions().len(), 1);

        let function = &parsed.server_functions()[0];
        assert!(function.body.as_sql_query().is_none());
        let select = function
            .body
            .as_no_input_parameter_select()
            .expect("f must use a no-input parameter select body");
        let select_start = source.find("SELECT").expect("SELECT exists");
        assert_eq!(select.source.text, "SELECT p_value");
        assert_eq!(
            select.source.span,
            SourceSpan {
                start: select_start,
                end: select_start + "SELECT p_value".len(),
            }
        );
        assert_eq!(select.parameter.text, "p_value");
        let parameter_start = source.rfind("p_value").expect("parameter exists");
        assert_eq!(
            select.parameter.span,
            SourceSpan {
                start: parameter_start,
                end: parameter_start + "p_value".len(),
            }
        );
        assert_eq!(
            &source[select.parameter.span.start..select.parameter.span.end],
            "p_value"
        );
    }

    #[test]
    fn keeps_rejecting_no_from_select_bodies_outside_the_exact_shape() {
        for (body, message) in [
            (
                "SELECT TRUE",
                "the current Orna SELECT parser does not yet implement SELECT query bodies without FROM; expected FROM followed by an aliased object source",
            ),
            (
                "SELECT NULL",
                "the current Orna SELECT parser does not yet implement bare alias expressions; expected a field path",
            ),
            (
                "SELECT p_value + 1",
                "the current Orna SELECT parser does not yet implement bare alias expressions; expected a field path",
            ),
            ("SELECT 1", "expected a query expression in SELECT query"),
            (
                "SELECT p_value, p_value",
                "the current Orna SELECT parser does not yet implement bare alias expressions; expected a field path",
            ),
            (
                "SELECT DISTINCT p_value",
                "the current Orna SELECT parser does not yet implement bare alias expressions; expected a field path",
            ),
            (
                "SELECT p_value WHERE p_value = 1",
                "the current Orna SELECT parser does not yet implement bare alias expressions; expected a field path",
            ),
            (
                "SELECT p_value ORDER BY p_value",
                "the current Orna SELECT parser does not yet implement bare alias expressions; expected a field path",
            ),
        ] {
            let source = format!("CREATE SERVER FUNCTION tasks.bad() RETURNS TEXT AS {body};");
            let parsed = parse(&source);

            assert_eq!(parsed.syntax().text(), source, "body: {body}");
            assert!(parsed.server_functions().is_empty(), "body: {body}");
            assert_eq!(parsed.diagnostics().len(), 1, "body: {body}");
            assert_eq!(parsed.diagnostics()[0].code, "ORNA0001", "body: {body}");
            assert_eq!(parsed.diagnostics()[0].message, message, "body: {body}");
        }
    }

    #[test]
    fn keeps_parsing_from_queries_as_sql_query_bodies() {
        let source = "CREATE SERVER FUNCTION tasks.list() RETURNS ROWS (task REF tasks.task) AS SELECT t.title FROM tasks.task t;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        let body = &parsed.server_functions()[0].body;
        assert!(body.as_no_input_parameter_select().is_none());
        let query = body
            .as_sql_query()
            .expect("tasks.list must use a SELECT body");
        assert_eq!(query.source.text, "SELECT t.title FROM tasks.task t");
        assert_eq!(query.query.source_object.alias.text, "t");
    }

    #[test]
    fn rejects_order_by_after_an_identity_selector_parameter() {
        let source = "CREATE SERVER FUNCTION tasks.bad(p_task REF tasks.task) RETURNS ROWS (task REF tasks.task) AS SELECT REF(selected) FROM tasks.task selected WHERE REF(selected) = p_task ORDER BY selected.title;";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert!(parsed.server_functions().is_empty());
        assert_eq!(parsed.diagnostics().len(), 1);
        let diagnostic = &parsed.diagnostics()[0];
        assert_eq!(diagnostic.code, "ORNA0001");
        assert_eq!(
            diagnostic.message,
            "identity-selected SELECT queries do not allow ORDER BY; remove the ORDER BY clause",
        );
        let order_start = source.find("ORDER BY").expect("ORDER BY exists");
        assert_eq!(
            diagnostic.span,
            SourceSpan {
                start: order_start,
                end: order_start + "ORDER".len(),
            }
        );
    }

    #[test]
    fn recovers_to_later_declarations_after_an_invalid_identity_selector() {
        let source = "CREATE SERVER FUNCTION tasks.bad(p_task REF tasks.task) RETURNS ROWS (task REF tasks.task) AS SELECT REF(selected) FROM tasks.task selected WHERE p_task = REF(selected);\n\
            CREATE SERVER FUNCTION tasks.good(p_task REF tasks.task) RETURNS ROWS (task REF tasks.task) AS SELECT REF(selected) FROM tasks.task selected WHERE REF(selected) = p_task;";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(parsed.server_functions().len(), 1);
        assert_eq!(parsed.server_functions()[0].name.parts[1].text, "good");
    }

    #[test]
    fn reports_malformed_and_unimplemented_select_bodies_without_losing_recovery() {
        let malformed =
            "CREATE SERVER FUNCTION tasks.bad() RETURNS TEXT AS SELECT REF() FROM tasks.task t;";
        let parsed = parse(malformed);
        assert_eq!(parsed.syntax().text(), malformed);
        assert!(parsed.server_functions().is_empty());
        assert!(parsed.diagnostics().iter().any(|diagnostic| {
            diagnostic.code == "ORNA0001" && diagnostic.message.contains("alias inside REF")
        }));

        let unsupported = "CREATE SERVER FUNCTION tasks.unsupported() RETURNS TEXT AS SELECT t.* FROM tasks.task t;\n\
            CREATE SERVER FUNCTION tasks.ok() RETURNS TEXT AS SELECT t.title FROM tasks.task t;";
        let parsed = parse(unsupported);
        assert_eq!(parsed.syntax().text(), unsupported);
        assert_eq!(parsed.server_functions().len(), 1);
        assert_eq!(parsed.server_functions()[0].name.parts[1].text, "ok");
        assert_eq!(parsed.diagnostics().len(), 1);
        let diagnostic = &parsed.diagnostics()[0];
        assert_eq!(diagnostic.code, "ORNA0001");
        assert_eq!(
            diagnostic.message,
            "the current Orna SELECT parser does not yet implement wildcard field paths; expected a field name after '.'"
        );
        let wildcard = unsupported.find('*').expect("wildcard exists");
        assert_eq!(diagnostic.span.start, wildcard);
        assert_eq!(diagnostic.span.end, wildcard + 1);
    }

    #[test]
    fn defers_query_alias_resolution_to_later_semantic_stages() {
        let source = "CREATE SERVER FUNCTION tasks.unresolved() RETURNS TEXT AS\n\
            SELECT REF(other), other.title FROM tasks.task t;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        let body = parsed.server_functions()[0]
            .body
            .as_sql_query()
            .expect("tasks.unresolved must use a SELECT body");
        assert!(matches!(
            body.query.projections[0],
            QueryExpression::ObjectReference { ref alias, .. } if alias.text == "other"
        ));
        assert!(matches!(
            body.query.projections[1],
            QueryExpression::FieldPath { ref root, .. } if root.text == "other"
        ));
    }

    #[test]
    fn parses_single_row_insert_bodies_losslessly() {
        let source = "CREATE SERVER FUNCTION tasks.create (\n\
            p_title TEXT,\n\
            p_done BOOL,\n\
            p_owner REF tasks.owner\n\
        )\n\
        RETURNS ROWS (created REF tasks.task)\n\
        SECURITY INVOKER\n\
        TRANSACTION ATOMIC\n\
        VOLATILITY VOLATILE\n\
        AS\n\
            INSERT /* target */ INTO tasks /* type */ . task AS created (\n\
                title, done, owner\n\
            ) VALUES (p_title, p_done, p_owner) RETURNING REF(created);";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        let function = &parsed.server_functions()[0];
        let body = function
            .body
            .as_sql_insert()
            .expect("the function must have an INSERT body");
        let insert = &body.insert;
        assert!(function.body.as_sql_query().is_none());
        let insert_start = source.find("INSERT").expect("INSERT exists");
        let body_end = source.rfind(")").expect("RETURNING close exists") + 1;
        assert_eq!(body.source.text, &source[insert_start..body_end]);
        assert_eq!(body.source.span.start, insert_start);
        assert_eq!(body.source.span.end, body_end);
        assert_eq!(insert.span, body.source.span);
        assert_eq!(insert.target_object.parts[0].text, "tasks");
        assert_eq!(insert.target_object.parts[1].text, "task");
        assert_eq!(insert.target_alias.text, "created");
        assert_eq!(insert.target_fields.len(), 3);
        assert_eq!(insert.target_fields[0].text, "title");
        assert_eq!(insert.target_fields[1].text, "done");
        assert_eq!(insert.target_fields[2].text, "owner");
        assert!(matches!(
            &insert.values[0],
            InsertValue::Parameter(name) if name.text == "p_title"
        ));
        assert!(matches!(
            &insert.values[1],
            InsertValue::Parameter(name) if name.text == "p_done"
        ));
        assert!(matches!(
            &insert.values[2],
            InsertValue::Parameter(name) if name.text == "p_owner"
        ));
        assert_eq!(insert.returning_alias.text, "created");
        assert_eq!(
            insert.returning_alias.span.start,
            source.rfind("created").unwrap()
        );
        assert_eq!(
            insert.values[0].span().start,
            source.rfind("p_title").unwrap()
        );
    }

    #[test]
    fn retains_insert_returning_ref_span_with_trivia() {
        let source = "cReAtE sErVeR fUnCtIoN t.i(p TEXT) ReTuRnS rOwS (r REF t.o) aS iNsErT /* target */ iNtO t.o aS r (x) vAlUeS (p) rEtUrNiNg rEf( /* before close */ r /* after */ );";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        let insert = &parsed.server_functions()[0]
            .body
            .as_sql_insert()
            .expect("the function must have an INSERT body")
            .insert;
        assert_eq!(insert.target_alias.text, "r");
        assert_eq!(insert.values.len(), 1);
        assert_eq!(insert.returning_alias.text, "r");
        assert_eq!(
            insert.returning_ref_span,
            SourceSpan {
                start: 122,
                end: 161,
            }
        );
        assert_eq!(
            &source[insert.returning_ref_span.start..insert.returning_ref_span.end],
            "rEf( /* before close */ r /* after */ )"
        );
    }

    #[test]
    fn parses_record_constructors_in_insert_values_losslessly() {
        let source = "CREATE SERVER FUNCTION tasks.create(p_x INT, p_stage tasks.stage) RETURNS ROWS (result REF tasks.item) AS INSERT INTO tasks.item AS made (point) VALUES (tasks.point{stage: p_stage, /* reordered */ x: p_x, ready: TRUE,}) RETURNING REF(made);";
        let parsed = parse(source);

        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        assert_eq!(parsed.syntax().text(), source);
        let insert = &parsed.server_functions()[0]
            .body
            .as_sql_insert()
            .expect("the function must have an INSERT body")
            .insert;
        let InsertValue::RecordConstructor(constructor) = &insert.values[0] else {
            panic!("the INSERT value must be a record constructor");
        };
        assert_eq!(constructor.record_type.parts[0].text, "tasks");
        assert_eq!(constructor.record_type.parts[1].text, "point");
        assert_eq!(constructor.fields.len(), 3);
        assert_eq!(constructor.fields[0].name.text, "stage");
        assert!(matches!(
            &constructor.fields[0].value,
            RecordConstructorFieldValue::Parameter(parameter) if parameter.text == "p_stage"
        ));
        assert_eq!(constructor.fields[1].name.text, "x");
        assert!(matches!(
            &constructor.fields[1].value,
            RecordConstructorFieldValue::Parameter(parameter) if parameter.text == "p_x"
        ));
        assert_eq!(constructor.fields[2].name.text, "ready");
        assert!(matches!(
            &constructor.fields[2].value,
            RecordConstructorFieldValue::BooleanLiteral { value: true, source }
                if source.text == "TRUE"
        ));
        let constructor_start = source.find("tasks.point{").unwrap();
        let constructor_end = source.find("}) RETURNING").unwrap() + 1;
        assert_eq!(
            constructor.span,
            SourceSpan {
                start: constructor_start,
                end: constructor_end,
            }
        );
        assert_eq!(insert.values[0].span(), &constructor.span);
        assert_eq!(
            constructor.fields[1].span,
            SourceSpan {
                start: source.find("x: p_x").unwrap(),
                end: source.find("p_x, ready").unwrap() + "p_x".len(),
            }
        );
    }

    #[test]
    fn record_constructor_diagnostics_close_the_initial_expression_subset() {
        let cases = [
            (
                "tasks.point{x: NULL}",
                "record constructor fields accept only a declared parameter, TRUE, or FALSE",
                "NULL",
            ),
            (
                "tasks.point{x: make_x()}",
                "record constructor fields do not support function calls",
                "(",
            ),
            (
                "tasks.point{x: other.value}",
                "record constructor fields do not support field paths or qualified values",
                ".",
            ),
            (
                "tasks.point{x: tasks.inner{x: p_x}}",
                "record constructor fields do not support nested record constructors",
                "{x: p_x}",
            ),
            (
                "tasks.point{x: p_x, X: p_x}",
                "record constructor field x appears more than once",
                "X: p_x",
            ),
        ];

        for (value, message, marker) in cases {
            let source = format!(
                "CREATE SERVER FUNCTION tasks.bad(p_x INT) RETURNS ROWS (result REF tasks.item) AS INSERT INTO tasks.item AS made (point) VALUES ({value}) RETURNING REF(made);"
            );
            let parsed = parse(&source);
            assert!(parsed.server_functions().is_empty(), "{value}");
            assert_eq!(parsed.diagnostics().len(), 1, "{value}");
            assert_eq!(parsed.diagnostics()[0].code, "ORNA0001", "{value}");
            assert_eq!(parsed.diagnostics()[0].message, message, "{value}");
            let value_start = source.find(value).unwrap();
            let marker_start = value_start + value.rfind(marker).unwrap();
            assert_eq!(parsed.diagnostics()[0].span.start, marker_start, "{value}");
        }
    }

    #[test]
    fn update_values_do_not_accept_record_constructors() {
        let source = "CREATE SERVER FUNCTION tasks.update(p_item REF tasks.item, p_x INT) RETURNS ROWS (result REF tasks.item) AS UPDATE tasks.item AS item SET point = tasks.point{x: p_x} WHERE REF(item) = p_item RETURNING REF(item);";
        let parsed = parse(source);

        assert!(parsed.server_functions().is_empty());
        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(
            parsed.diagnostics()[0].message,
            "this UPDATE does not support record constructors in UPDATE values; expected a declared parameter name by itself"
        );
        assert_eq!(
            parsed.diagnostics()[0].span.start,
            source.find('{').unwrap()
        );
    }

    #[test]
    fn empty_record_constructor_recovers_to_a_later_function() {
        let source = "CREATE SERVER FUNCTION tasks.bad() RETURNS ROWS (result REF tasks.item) AS INSERT INTO tasks.item AS made (point) VALUES (tasks.point{}) RETURNING REF(made);\n\
            CREATE SERVER FUNCTION tasks.good(p_x INT) RETURNS ROWS (result REF tasks.item) AS INSERT INTO tasks.item AS made (point) VALUES (p_x) RETURNING REF(made);";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.server_functions().len(), 1);
        assert_eq!(parsed.server_functions()[0].name.parts[1].text, "good");
        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(
            parsed.diagnostics()[0].message,
            "record constructor must supply at least one field"
        );
        let close = source.find("{}").unwrap() + 1;
        assert_eq!(
            parsed.diagnostics()[0].span,
            SourceSpan {
                start: close,
                end: close + 1,
            }
        );
    }

    #[test]
    fn rejects_closed_insert_forms_and_recovers_to_a_valid_declaration() {
        let invalid = [
            "INSERT INTO tasks.task created (title) VALUES (p_title) RETURNING REF(created)",
            "INSERT INTO tasks.task AS created () VALUES (p_title) RETURNING REF(created)",
            "INSERT INTO tasks.task AS created (title,) VALUES (p_title) RETURNING REF(created)",
            "INSERT INTO tasks.task AS created (title, title) VALUES (p_title, p_title) RETURNING REF(created)",
            "INSERT INTO tasks.task AS created (title, \"title\") VALUES (p_title, p_title) RETURNING REF(created)",
            "INSERT INTO tasks.task AS created (tasks.title) VALUES (p_title) RETURNING REF(created)",
            "INSERT INTO tasks.task AS created (title, done) VALUES (p_title) RETURNING REF(created)",
            "INSERT INTO tasks.task AS created (title) VALUES (p_title, TRUE) RETURNING REF(created)",
            "INSERT INTO tasks.task AS created (title) VALUES () RETURNING REF(created)",
            "INSERT INTO tasks.task AS created (title) VALUES (p_title,) RETURNING REF(created)",
            "INSERT INTO tasks.task AS created (title) VALUES (p_title), (p_title) RETURNING REF(created)",
            "INSERT INTO tasks.task AS created (title) VALUES ('title') RETURNING REF(created)",
            "INSERT INTO tasks.task AS created (title) VALUES (make_title()) RETURNING REF(created)",
            "INSERT INTO tasks.task AS created (title) VALUES (other.title) RETURNING REF(created)",
            "INSERT INTO tasks.task AS created (title) VALUES (p_title) RETURNING created",
            "INSERT INTO tasks.task AS created (title) VALUES (p_title) RETURNING REF(created.title)",
            "INSERT INTO tasks.task AS created (title) VALUES (p_title) RETURNING REF(other)",
        ];
        for body in invalid {
            let source = format!(
                "CREATE SERVER FUNCTION tasks.bad(p_title TEXT) RETURNS ROWS (created REF tasks.task) AS {body};"
            );
            let parsed = parse(&source);
            assert_eq!(parsed.syntax().text(), source);
            assert!(parsed.server_functions().is_empty(), "invalid body: {body}");
            assert!(!parsed.diagnostics().is_empty(), "invalid body: {body}");
        }

        let source = "CREATE SERVER FUNCTION tasks.bad(p_title TEXT) RETURNS ROWS (created REF tasks.task) AS INSERT INTO tasks.task AS Created (title) VALUES (p_title) RETURNING REF(OTHER);\n\
            CREATE SERVER FUNCTION tasks.good(p_title TEXT) RETURNS ROWS (result REF tasks.task) AS INSERT INTO tasks.task AS created (title) VALUES (p_title) RETURNING REF(created);";
        let parsed = parse(source);
        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.server_functions().len(), 1);
        assert_eq!(parsed.server_functions()[0].name.parts[1].text, "good");
        assert_eq!(parsed.diagnostics().len(), 1);
        let diagnostic = &parsed.diagnostics()[0];
        assert_eq!(diagnostic.code, "ORNA0001");
        assert_eq!(
            diagnostic.message,
            "RETURNING REF must use the INSERT target alias created, not other"
        );
        let other = source.find("OTHER").expect("wrong alias exists");
        assert_eq!(diagnostic.span.start, other);
        assert_eq!(diagnostic.span.end, other + "OTHER".len());
    }

    #[test]
    fn insert_keywords_and_unquoted_aliases_are_case_insensitive() {
        let source = "CREATE SERVER FUNCTION tasks.create() RETURNS ROWS (result REF tasks.task) AS iNsErT iNtO tasks.task aS Created (done, note) vAlUeS (fAlSe, nUlL) rEtUrNiNg rEf(created);";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        let body = parsed.server_functions()[0]
            .body
            .as_sql_insert()
            .expect("the function must have an INSERT body");
        assert_eq!(body.insert.target_alias.text, "Created");
        assert_eq!(body.insert.returning_alias.text, "created");
        assert!(matches!(
            &body.insert.values[0],
            InsertValue::BooleanLiteral { value: false, source } if source.text == "fAlSe"
        ));
        assert!(matches!(
            &body.insert.values[1],
            InsertValue::NullLiteral { source } if source.text == "nUlL"
        ));
    }

    #[test]
    fn duplicate_insert_field_diagnostic_uses_the_normalised_name_and_exact_span() {
        let source = "CREATE SERVER FUNCTION tasks.bad(p_title TEXT) RETURNS ROWS (result REF tasks.task) AS INSERT INTO tasks.task AS created (Title, \"title\") VALUES (p_title, p_title) RETURNING REF(created);";
        let parsed = parse(source);

        assert!(parsed.server_functions().is_empty());
        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(parsed.diagnostics()[0].code, "ORNA0001");
        assert_eq!(
            parsed.diagnostics()[0].message,
            "field title appears more than once in this INSERT"
        );
        let duplicate = source.find("\"title\"").expect("duplicate field exists");
        assert_eq!(parsed.diagnostics()[0].span.start, duplicate);
        assert_eq!(
            parsed.diagnostics()[0].span.end,
            duplicate + "\"title\"".len()
        );
    }

    #[test]
    fn insert_count_diagnostics_use_grammatical_nouns_and_exact_spans() {
        let cases = [
            (
                "INSERT INTO tasks.task AS created (title, done) VALUES (p_title) RETURNING REF(created)",
                "INSERT lists 2 fields but 1 value; each field requires one value",
                ") RETURNING",
                1,
            ),
            (
                "INSERT INTO tasks.task AS created (title) VALUES (p_title, p_done) RETURNING REF(created)",
                "INSERT lists 1 field but 2 values; each field requires one value",
                "p_done) RETURNING",
                "p_done".len(),
            ),
        ];

        for (body, message, marker, span_length) in cases {
            assert_insert_diagnostic(body, message, marker, 0, span_length);
        }
    }

    #[test]
    fn qualified_insert_names_report_guidance_at_the_dot() {
        let cases = [
            (
                "INSERT INTO tasks.task AS created (tasks.title) VALUES (p_title) RETURNING REF(created)",
                "write only the field name in the INSERT field list; do not add an object or alias",
                "tasks.title",
                "tasks".len(),
            ),
            (
                "INSERT INTO tasks.task AS created (title) VALUES (other.p_title) RETURNING REF(created)",
                "use the declared parameter name by itself in VALUES; do not add an object or alias",
                "other.p_title",
                "other".len(),
            ),
        ];

        for (body, message, marker, dot_offset) in cases {
            assert_insert_diagnostic(body, message, marker, dot_offset, 1);
        }
    }

    #[test]
    fn insert_implementation_gap_diagnostics_use_exact_copy_and_spans() {
        let cases = [
            (
                "INSERT INTO tasks.task AS created (title) VALUES (p_title), (p_title) RETURNING REF(created)",
                "this INSERT does not support multiple VALUES rows; expected RETURNING after one VALUES row",
                ", (p_title) RETURNING",
                0,
                1,
            ),
            (
                "INSERT INTO tasks.task AS created (title) VALUES (p_title) RETURNING REF(created) EXTRA",
                "this INSERT does not support EXTRA; expected the end of the INSERT body",
                "EXTRA",
                0,
                "EXTRA".len(),
            ),
            (
                "INSERT INTO tasks.task AS created (title) VALUES (make_title()) RETURNING REF(created)",
                "this INSERT does not support function calls in INSERT values; expected a declared parameter name by itself",
                "make_title()",
                "make_title".len(),
                1,
            ),
        ];

        for (body, message, marker, span_offset, span_length) in cases {
            assert_insert_diagnostic(body, message, marker, span_offset, span_length);
        }
    }

    #[test]
    fn malformed_insert_quotes_report_diagnostics_without_panicking() {
        let source = "CREATE SERVER FUNCTION tasks.bad(p_title TEXT) RETURNS ROWS (result REF tasks.task) AS INSERT INTO tasks.task AS created (title) VALUES (p_title) RETURNING REF(\"";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert!(parsed.server_functions().is_empty());
        assert!(parsed.diagnostics().iter().any(|diagnostic| {
            diagnostic.code == "ORNA0002" && diagnostic.message == "unterminated quoted identifier"
        }));
    }

    #[test]
    fn malformed_insert_parentheses_do_not_consume_later_declarations() {
        let source = "CREATE SERVER FUNCTION tasks.bad(p_title TEXT) RETURNS ROWS (result REF tasks.task) AS INSERT INTO tasks.task AS created (title) VALUES (p_title;
            CREATE SERVER FUNCTION tasks.good(p_title TEXT) RETURNS ROWS (result REF tasks.task) AS INSERT INTO tasks.task AS created (title) VALUES (p_title) RETURNING REF(created);";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.server_functions().len(), 1);
        assert_eq!(parsed.server_functions()[0].name.parts[1].text, "good");
        assert!(parsed.diagnostics().iter().any(|diagnostic| {
            diagnostic.code == "ORNA0001"
                && diagnostic
                    .message
                    .contains("expected ',' or ')' after an INSERT value")
        }));
    }

    #[test]
    fn parses_single_object_update_bodies_losslessly() {
        let source = "CREATE SERVER FUNCTION tasks.update(
            p_task REF tasks.task,
            p_title TEXT
        )
        RETURNS ROWS (updated REF tasks.task)
        SECURITY INVOKER
        TRANSACTION ATOMIC
        VOLATILITY VOLATILE
        AS UPDATE /* target */ tasks.task AS Updated
            SET title = p_title, done = FALSE, note = NULL
            WHERE REF(updated) = p_task
            RETURNING REF(UPDATED);";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        let function = &parsed.server_functions()[0];
        let body = function
            .body
            .as_sql_update()
            .expect("the function must have an UPDATE body");
        assert!(function.body.as_sql_query().is_none());
        assert!(function.body.as_sql_insert().is_none());
        let update_start = source.find("UPDATE /* target */").unwrap();
        let body_end = source.rfind(')').unwrap() + 1;
        assert_eq!(body.source.text, &source[update_start..body_end]);
        assert_eq!(
            body.source.span,
            SourceSpan {
                start: update_start,
                end: body_end
            }
        );
        assert_eq!(body.update.span, body.source.span);
        assert_eq!(body.update.target_object.parts[0].text, "tasks");
        assert_eq!(body.update.target_object.parts[1].text, "task");
        assert_eq!(body.update.target_alias.text, "Updated");
        assert_eq!(body.update.assignments.len(), 3);
        assert_eq!(body.update.assignments[0].target_field.text, "title");
        assert!(matches!(
            &body.update.assignments[0].value,
            MutationValue::Parameter(name) if name.text == "p_title"
        ));
        assert!(matches!(
            &body.update.assignments[1].value,
            MutationValue::BooleanLiteral { value: false, source } if source.text == "FALSE"
        ));
        assert!(matches!(
            &body.update.assignments[2].value,
            MutationValue::NullLiteral { source } if source.text == "NULL"
        ));
        assert_eq!(body.update.selector_alias.text, "updated");
        assert_eq!(body.update.selector_parameter.text, "p_task");
        assert_eq!(body.update.returning_alias.text, "UPDATED");
        assert_eq!(
            body.update.assignments[0].span.start,
            source.find("title = p_title").unwrap()
        );
        assert_eq!(
            body.update.assignments[0].span.end,
            source.find("p_title, done").unwrap() + "p_title".len()
        );
    }

    #[test]
    fn retains_update_selector_and_returning_ref_spans_with_trivia() {
        let source = "cReAtE sErVeR fUnCtIoN t.u(p REF t.o, x TEXT) ReTuRnS rOwS (r REF t.o) aS uPdAtE /* target */ t.o aS r SeT x = p wHeRe rEf( /* selector */ r /* close */ ) /* equals */ = /* parameter */ p rEtUrNiNg rEf( /* returning */ r /* close */ );";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        let update = &parsed.server_functions()[0]
            .body
            .as_sql_update()
            .expect("the function must have an UPDATE body")
            .update;
        assert_eq!(update.target_alias.text, "r");
        assert_eq!(update.assignments.len(), 1);
        assert_eq!(update.selector_alias.text, "r");
        assert_eq!(update.selector_parameter.text, "p");
        assert_eq!(update.returning_alias.text, "r");
        assert_eq!(
            update.selector_ref_span,
            SourceSpan {
                start: 119,
                end: 154,
            }
        );
        assert_eq!(
            update.selector_equality_span,
            SourceSpan {
                start: 119,
                end: 187,
            }
        );
        assert_eq!(
            update.returning_ref_span,
            SourceSpan {
                start: 198,
                end: 234,
            }
        );
        assert_eq!(
            &source[update.selector_ref_span.start..update.selector_ref_span.end],
            "rEf( /* selector */ r /* close */ )"
        );
        assert_eq!(
            &source[update.selector_equality_span.start..update.selector_equality_span.end],
            "rEf( /* selector */ r /* close */ ) /* equals */ = /* parameter */ p"
        );
        assert_eq!(
            &source[update.returning_ref_span.start..update.returning_ref_span.end],
            "rEf( /* returning */ r /* close */ )"
        );
    }

    #[test]
    fn update_diagnostics_are_direct_and_select_the_offending_source() {
        let cases = [
            (
                "UPDATE tasks.task AS updated SET Title = p_title, \"title\" = p_title WHERE REF(updated) = p_task RETURNING REF(updated)",
                "field title appears more than once in this UPDATE",
                "\"title\" =",
                0,
                "\"title\"".len(),
            ),
            (
                "UPDATE tasks.task AS updated SET tasks.title = p_title WHERE REF(updated) = p_task RETURNING REF(updated)",
                "write only the field name in SET; do not add an object or alias",
                "tasks.title",
                "tasks".len(),
                1,
            ),
            (
                "UPDATE tasks.task AS updated SET title = input.p_title WHERE REF(updated) = p_task RETURNING REF(updated)",
                "use the declared parameter name by itself after '='; do not add an object or alias",
                "input.p_title",
                "input".len(),
                1,
            ),
            (
                "UPDATE tasks.task AS updated SET title = p_title WHERE REF(other) = p_task RETURNING REF(updated)",
                "WHERE REF must use the UPDATE target alias updated, not other",
                "REF(other)",
                "REF(".len(),
                "other".len(),
            ),
            (
                "UPDATE tasks.task AS updated SET title = p_title WHERE REF(updated) = p_task RETURNING REF(other)",
                "RETURNING REF must use the UPDATE target alias updated, not other",
                "REF(other)",
                "REF(".len(),
                "other".len(),
            ),
            (
                "UPDATE tasks.task AS updated SET title = make_title() WHERE REF(updated) = p_task RETURNING REF(updated)",
                "this UPDATE does not support function calls in UPDATE values; expected a declared parameter name by itself",
                "make_title()",
                "make_title".len(),
                1,
            ),
            (
                "UPDATE tasks.task AS updated SET title = p_title WHERE REF(updated) = p_task RETURNING REF(updated) EXTRA",
                "this UPDATE does not support EXTRA; expected the end of the UPDATE body",
                "EXTRA",
                0,
                "EXTRA".len(),
            ),
        ];

        for (body, message, marker, span_offset, span_length) in cases {
            assert_update_diagnostic(body, message, marker, span_offset, span_length);
        }
    }

    #[test]
    fn rejects_closed_update_forms_and_recovers_to_a_later_declaration() {
        let cases = [
            (
                "UPDATE tasks.task updated SET title = p_title WHERE REF(updated) = p_task RETURNING REF(updated)",
                "expected AS before the UPDATE target alias in UPDATE body",
                "updated SET",
                0,
                "updated".len(),
            ),
            (
                "UPDATE tasks.task AS updated SET WHERE REF(updated) = p_task RETURNING REF(updated)",
                "expected at least one field assignment after SET in UPDATE body",
                "WHERE",
                0,
                "WHERE".len(),
            ),
            (
                "UPDATE tasks.task AS updated SET title p_title WHERE REF(updated) = p_task RETURNING REF(updated)",
                "expected '=' after the UPDATE field name in UPDATE body",
                "p_title WHERE",
                0,
                "p_title".len(),
            ),
            (
                "UPDATE tasks.task AS updated SET title = p_title, WHERE REF(updated) = p_task RETURNING REF(updated)",
                "expected a field assignment after ',' in UPDATE body",
                "WHERE",
                0,
                "WHERE".len(),
            ),
            (
                "UPDATE tasks.task AS updated SET title = p_title WHERE updated.id = p_task RETURNING REF(updated)",
                "expected REF(target_alias) after WHERE in UPDATE body",
                "updated.id",
                0,
                "updated".len(),
            ),
            (
                "UPDATE tasks.task AS updated SET title = p_title WHERE REF(updated) = TRUE RETURNING REF(updated)",
                "expected a declared REF parameter after '=' in UPDATE body",
                "TRUE",
                0,
                "TRUE".len(),
            ),
            (
                "UPDATE tasks.task AS updated SET title = p_title WHERE REF(updated) = owner.p_task RETURNING REF(updated)",
                "use the selector parameter name by itself after '='; do not add an object or alias",
                "owner.p_task",
                "owner".len(),
                1,
            ),
            (
                "UPDATE tasks.task AS updated SET title = p_title WHERE REF(updated) = find_task() RETURNING REF(updated)",
                "this UPDATE does not support function calls as UPDATE selectors; expected a declared REF parameter name by itself",
                "find_task()",
                "find_task".len(),
                1,
            ),
            (
                "UPDATE tasks.task AS updated SET title = p_title WHERE REF(updated) = p_task RETURNING updated",
                "expected REF in the RETURNING expression in UPDATE body",
                "RETURNING updated",
                "RETURNING ".len(),
                "updated".len(),
            ),
        ];
        for (body, message, marker, span_offset, span_length) in cases {
            assert_update_diagnostic(body, message, marker, span_offset, span_length);
        }

        let source = "CREATE SERVER FUNCTION tasks.bad(p_task REF tasks.task, p_title TEXT) RETURNS ROWS (updated REF tasks.task) AS UPDATE tasks.task AS updated SET title = p_title WHERE REF(other) = p_task RETURNING REF(updated);
            CREATE SERVER FUNCTION tasks.good(p_task REF tasks.task, p_title TEXT) RETURNS ROWS (updated REF tasks.task) AS UPDATE tasks.task AS updated SET title = p_title WHERE REF(updated) = p_task RETURNING REF(updated);";
        let parsed = parse(source);
        assert_eq!(parsed.server_functions().len(), 1);
        assert_eq!(parsed.server_functions()[0].name.parts[1].text, "good");
        assert!(parsed.server_functions()[0].body.as_sql_update().is_some());
        assert_eq!(parsed.diagnostics().len(), 1);
    }

    #[test]
    fn malformed_update_parentheses_do_not_consume_later_declarations() {
        let source = "CREATE SERVER FUNCTION tasks.bad(p_task REF tasks.task, p_title TEXT) RETURNS ROWS (updated REF tasks.task) AS UPDATE tasks.task AS updated SET title = p_title WHERE REF(updated = p_task RETURNING REF(updated);
            CREATE SERVER FUNCTION tasks.good(p_task REF tasks.task, p_title TEXT) RETURNS ROWS (updated REF tasks.task) AS UPDATE tasks.task AS updated SET title = p_title WHERE REF(updated) = p_task RETURNING REF(updated);";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.server_functions().len(), 1);
        assert_eq!(parsed.server_functions()[0].name.parts[1].text, "good");
        assert!(parsed.diagnostics().iter().any(|diagnostic| {
            diagnostic.code == "ORNA0001"
                && diagnostic.message == "expected ')' after the WHERE REF alias in UPDATE body"
        }));
    }

    #[test]
    fn parses_single_object_delete_bodies_losslessly() {
        let source = "CREATE SERVER FUNCTION tasks.remove(p_task REF tasks.task)
            RETURNS ROWS (deleted BOOL)
            SECURITY INVOKER TRANSACTION ATOMIC VOLATILITY VOLATILE
            AS DELETE /* target */ FROM tasks.task AS \"Gone\"
            WHERE REF(\"Gone\") = p_task
            RETURNING TrUe;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        let function = &parsed.server_functions()[0];
        let body = function
            .body
            .as_sql_delete()
            .expect("the function must have a DELETE body");
        assert!(function.body.as_sql_query().is_none());
        assert!(function.body.as_sql_insert().is_none());
        assert!(function.body.as_sql_update().is_none());
        let delete_start = source.find("DELETE /* target */").unwrap();
        let body_end = source.rfind("TrUe").unwrap() + "TrUe".len();
        assert_eq!(body.source.text, &source[delete_start..body_end]);
        assert_eq!(
            body.source.span,
            SourceSpan {
                start: delete_start,
                end: body_end,
            }
        );
        assert_eq!(body.delete.span, body.source.span);
        assert_eq!(body.delete.target_object.parts[0].text, "tasks");
        assert_eq!(body.delete.target_object.parts[1].text, "task");
        assert_eq!(body.delete.target_alias.text, "\"Gone\"");
        assert_eq!(body.delete.selector_alias.text, "\"Gone\"");
        assert_eq!(body.delete.selector_parameter.text, "p_task");
        assert_eq!(body.delete.returning_true.text, "TrUe");
        assert_eq!(
            body.delete.returning_true.span.start,
            source.rfind("TrUe").unwrap()
        );
        assert_eq!(body.delete.returning_true.span.end, body_end);
    }

    #[test]
    fn retains_delete_selector_spans_with_trivia() {
        let source = "cReAtE sErVeR fUnCtIoN t.d(p REF t.o) ReTuRnS rOwS (d bOoL) aS dElEtE /* target */ fRoM t.o aS r wHeRe rEf( /* selector */ r /* close */ ) /* equals */ = /* parameter */ p rEtUrNiNg tRuE;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        let delete = &parsed.server_functions()[0]
            .body
            .as_sql_delete()
            .expect("the function must have a DELETE body")
            .delete;
        assert_eq!(delete.target_alias.text, "r");
        assert_eq!(delete.selector_alias.text, "r");
        assert_eq!(delete.selector_parameter.text, "p");
        assert_eq!(delete.returning_true.text, "tRuE");
        assert_eq!(
            delete.selector_ref_span,
            SourceSpan {
                start: 103,
                end: 138,
            }
        );
        assert_eq!(
            delete.selector_equality_span,
            SourceSpan {
                start: 103,
                end: 171,
            }
        );
        assert_eq!(
            &source[delete.selector_ref_span.start..delete.selector_ref_span.end],
            "rEf( /* selector */ r /* close */ )"
        );
        assert_eq!(
            &source[delete.selector_equality_span.start..delete.selector_equality_span.end],
            "rEf( /* selector */ r /* close */ ) /* equals */ = /* parameter */ p"
        );
    }

    #[test]
    fn delete_diagnostics_are_exact_and_select_the_offending_source() {
        let cases = [
            (
                "DELETE tasks.task AS deleted_task WHERE REF(deleted_task) = p_task RETURNING TRUE",
                "expected FROM after DELETE in DELETE body",
                "DELETE tasks.task",
                "DELETE ".len(),
                "tasks".len(),
            ),
            (
                "DELETE FROM tasks.task deleted_task WHERE REF(deleted_task) = p_task RETURNING TRUE",
                "expected AS before the DELETE target alias in DELETE body",
                "deleted_task WHERE",
                0,
                "deleted_task".len(),
            ),
            (
                "DELETE FROM tasks.task AS deleted_task RETURNING TRUE",
                "expected WHERE after the DELETE target alias in DELETE body",
                "RETURNING",
                0,
                "RETURNING".len(),
            ),
            (
                "DELETE FROM tasks.task AS deleted_task WHERE deleted_task.id = p_task RETURNING TRUE",
                "expected REF(target_alias) after WHERE in DELETE body",
                "deleted_task.id",
                0,
                "deleted_task".len(),
            ),
            (
                "DELETE FROM tasks.task AS deleted_task WHERE REF(other) = p_task RETURNING TRUE",
                "WHERE REF must use the DELETE target alias deleted_task, not other",
                "REF(other)",
                "REF(".len(),
                "other".len(),
            ),
            (
                "DELETE FROM tasks.task AS deleted_task WHERE REF(deleted_task) p_task RETURNING TRUE",
                "expected '=' after WHERE REF(target_alias) in DELETE body",
                "p_task RETURNING",
                0,
                "p_task".len(),
            ),
            (
                "DELETE FROM tasks.task AS deleted_task WHERE REF(deleted_task) = TRUE RETURNING TRUE",
                "expected a declared REF parameter after '=' in DELETE body",
                "TRUE RETURNING",
                0,
                "TRUE".len(),
            ),
            (
                "DELETE FROM tasks.task AS deleted_task WHERE REF(deleted_task) = owner.p_task RETURNING TRUE",
                "use the selector parameter name by itself after '='; do not add an object or alias",
                "owner.p_task",
                "owner".len(),
                1,
            ),
            (
                "DELETE FROM tasks.task AS deleted_task WHERE REF(deleted_task) = find_task() RETURNING TRUE",
                "this DELETE does not support function calls as DELETE selectors; expected a declared REF parameter name by itself",
                "find_task()",
                "find_task".len(),
                1,
            ),
            (
                "DELETE FROM tasks.task AS deleted_task WHERE REF(deleted_task) = p_task EXTRA",
                "expected RETURNING after the DELETE selector in DELETE body",
                "EXTRA",
                0,
                "EXTRA".len(),
            ),
            (
                "DELETE FROM tasks.task AS deleted_task WHERE REF(deleted_task) = p_task RETURNING FALSE",
                "expected TRUE after RETURNING in DELETE body",
                "FALSE",
                0,
                "FALSE".len(),
            ),
            (
                "DELETE FROM tasks.task AS deleted_task WHERE REF(deleted_task) = p_task RETURNING REF(deleted_task)",
                "expected TRUE after RETURNING in DELETE body",
                "RETURNING REF(deleted_task)",
                "RETURNING ".len(),
                "REF".len(),
            ),
            (
                "DELETE FROM tasks.task AS deleted_task WHERE REF(deleted_task) = p_task RETURNING TRUE EXTRA",
                "this DELETE does not support EXTRA; expected the end of the DELETE body",
                "EXTRA",
                0,
                "EXTRA".len(),
            ),
        ];

        for (body, message, marker, span_offset, span_length) in cases {
            assert_delete_diagnostic(body, message, marker, span_offset, span_length);
        }
    }

    #[test]
    fn malformed_delete_parentheses_do_not_consume_later_declarations() {
        let source = "CREATE SERVER FUNCTION tasks.bad(p_task REF tasks.task) RETURNS ROWS (deleted BOOL) AS DELETE FROM tasks.task AS deleted_task WHERE REF(deleted_task = p_task RETURNING TRUE;
            CREATE SERVER FUNCTION tasks.good(p_task REF tasks.task) RETURNS ROWS (deleted BOOL) AS DELETE FROM tasks.task AS deleted_task WHERE REF(deleted_task) = p_task RETURNING TRUE;";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.server_functions().len(), 1);
        assert_eq!(parsed.server_functions()[0].name.parts[1].text, "good");
        assert!(parsed.server_functions()[0].body.as_sql_delete().is_some());
        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(parsed.diagnostics()[0].code, "ORNA0001");
        assert_eq!(
            parsed.diagnostics()[0].message,
            "expected ')' after the WHERE REF alias in DELETE body"
        );
    }

    #[test]
    fn parses_object_type_fields_without_rewriting_aliases_or_defaults() {
        let source = "CREATE TYPE tasks.task AS OBJECT (\n\
            title TEXT NOT NULL,\n\
            project REF tasks.project ON DELETE CASCADE,\n\
            completed BOOL NOT NULL DEFAULT FALSE,\n\
            object_id INT UNIQUE\n\
        );";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.object_types().len(), 1);

        let object_type = &parsed.object_types()[0];
        assert_eq!(object_type.name.parts[0].text, "tasks");
        assert_eq!(object_type.name.parts[1].text, "task");
        assert_eq!(object_type.fields.len(), 4);

        let title = &object_type.fields[0];
        assert_eq!(title.name.text, "title");
        assert_eq!(title.order, 0);
        assert!(!title.nullable);
        assert!(!title.unique);
        assert_named_type(&title.type_specification, "TEXT");
        assert_eq!(
            title.span.start,
            source.find("title").expect("title exists")
        );
        assert_eq!(
            title.span.end,
            source.find("NOT NULL").expect("NOT NULL exists") + 8
        );

        let project = &object_type.fields[1];
        assert!(project.nullable);
        assert_eq!(project.on_delete, Some(OnDeletePolicy::Cascade));
        match &project.type_specification {
            TypeSpecification::Reference { target, .. } => {
                let TypeSpecification::Named(target) = target.as_ref() else {
                    panic!("project reference target must be named");
                };
                assert_eq!(target.parts[0].text, "tasks");
                assert_eq!(target.parts[1].text, "project");
            }
            _ => panic!("project must be a reference"),
        }

        let completed = &object_type.fields[2];
        assert_named_type(&completed.type_specification, "BOOL");
        assert!(!completed.nullable);
        assert_eq!(
            completed
                .default_expression
                .as_ref()
                .map(|expression| expression.text.as_str()),
            Some("FALSE")
        );

        let object_id = &object_type.fields[3];
        assert_eq!(object_id.name.text, "object_id");
        assert_named_type(&object_id.type_specification, "INT");
        assert!(object_id.unique);
    }

    #[test]
    fn parses_each_supported_on_delete_policy() {
        let source = "CREATE TYPE crm.contact AS OBJECT (\n\
            restricted REF crm.person ON DELETE RESTRICT,\n\
            cleared REF crm.organisation ON DELETE SET NULL,\n\
            cascading REF crm.account ON DELETE CASCADE\n\
        );";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        let fields = &parsed.object_types()[0].fields;
        assert_eq!(fields[0].on_delete, Some(OnDeletePolicy::Restrict));
        assert_eq!(fields[1].on_delete, Some(OnDeletePolicy::SetNull));
        assert_eq!(fields[2].on_delete, Some(OnDeletePolicy::Cascade));
    }

    #[test]
    fn parses_simple_and_qualified_field_rename_declarations() {
        let source = "ALTER TYPE person RENAME FIELD email TO primary_email;\n\
            ALTER TYPE people.person RENAME FIELD email TO primary_email;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.field_renames().len(), 2);

        let simple = &parsed.field_renames()[0];
        assert_eq!(simple.type_name.parts.len(), 1);
        assert_eq!(simple.type_name.parts[0].text, "person");
        assert_eq!(simple.old_field_name.text, "email");
        assert_eq!(simple.new_field_name.text, "primary_email");
        assert_eq!(simple.span.start, 0);
        assert_eq!(
            simple.span.end,
            source.find('\n').expect("first declaration ends")
        );

        let qualified = &parsed.field_renames()[1];
        assert_eq!(qualified.type_name.parts[0].text, "people");
        assert_eq!(qualified.type_name.parts[1].text, "person");
        assert_eq!(qualified.old_field_name.text, "email");
        assert_eq!(qualified.new_field_name.text, "primary_email");
    }

    #[test]
    fn preserves_quoted_field_rename_identifiers_and_spans() {
        let source =
            "ALTER TYPE \"People\".\"Person\" RENAME FIELD \"Email\" TO \"Primary\"\"Email\";";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        let rename = &parsed.field_renames()[0];
        assert_eq!(rename.type_name.parts[0].text, "\"People\"");
        assert_eq!(rename.type_name.parts[1].text, "\"Person\"");
        assert_eq!(rename.old_field_name.text, "\"Email\"");
        assert_eq!(rename.new_field_name.text, "\"Primary\"\"Email\"");
        let people_start = source.find("\"People\"").unwrap();
        let person_start = source.find("\"Person\"").unwrap();
        let old_start = source.find("\"Email\"").unwrap();
        let new_start = source.find("\"Primary\"\"Email\"").unwrap();
        assert_eq!(
            rename.type_name.parts[0].span,
            SourceSpan {
                start: people_start,
                end: people_start + "\"People\"".len(),
            }
        );
        assert_eq!(
            rename.type_name.parts[1].span,
            SourceSpan {
                start: person_start,
                end: person_start + "\"Person\"".len(),
            }
        );
        assert_eq!(
            rename.type_name.span,
            SourceSpan {
                start: people_start,
                end: person_start + "\"Person\"".len(),
            }
        );
        assert_eq!(
            rename.old_field_name.span,
            SourceSpan {
                start: old_start,
                end: old_start + "\"Email\"".len(),
            }
        );
        assert_eq!(
            rename.new_field_name.span,
            SourceSpan {
                start: new_start,
                end: source.len() - 1,
            }
        );
    }

    #[test]
    fn reports_field_rename_syntax_errors_with_exact_diagnostics() {
        let cases = [
            (
                "ALTER people.person RENAME FIELD email TO primary_email;",
                "ALTER must be followed by TYPE",
                "people",
            ),
            (
                "ALTER TYPE RENAME FIELD email TO primary_email;",
                "expected the type name after ALTER TYPE",
                "RENAME",
            ),
            (
                "ALTER TYPE people. RENAME FIELD email TO primary_email;",
                "expected the type name after '.'",
                "RENAME",
            ),
            (
                "ALTER TYPE people.person FIELD email TO primary_email;",
                "expected RENAME after the type name",
                "FIELD",
            ),
            (
                "ALTER TYPE people.person RENAME email TO primary_email;",
                "expected FIELD after RENAME",
                "email",
            ),
            (
                "ALTER TYPE people.person RENAME FIELD TO primary_email;",
                "expected the old field name after RENAME FIELD",
                "TO",
            ),
            (
                "ALTER TYPE people.person RENAME FIELD email primary_email;",
                "expected TO after the old field name",
                "primary_email",
            ),
            (
                "ALTER TYPE people.person RENAME FIELD email TO;",
                "expected the new field name after TO",
                ";",
            ),
            (
                "ALTER TYPE people.person RENAME FIELD email TO primary_email",
                "expected ';' after field rename declaration",
                "",
            ),
            (
                "ALTER TYPE people.person RENAME FIELD email TO primary_email EXTRA;",
                "expected ';' after field rename declaration",
                "EXTRA",
            ),
            (
                "ALTER SCHEMA people RENAME FIELD email TO primary_email;",
                "ALTER must be followed by TYPE",
                "SCHEMA",
            ),
            (
                "ALTER TYPE people.person RENAME TO person;",
                "ALTER TYPE only supports RENAME FIELD",
                "TO",
            ),
        ];

        for (source, message, marker) in cases {
            let parsed = parse(source);
            assert_eq!(parsed.syntax().text(), source);
            assert!(
                parsed.field_renames().is_empty(),
                "invalid source: {source}"
            );
            assert_eq!(parsed.diagnostics().len(), 1, "invalid source: {source}");
            let diagnostic = &parsed.diagnostics()[0];
            assert_eq!(diagnostic.code, "ORNA0001");
            assert_eq!(diagnostic.message, message);
            let offset = if marker.is_empty() {
                source.len()
            } else {
                source.find(marker).expect("diagnostic marker exists")
            };
            assert_eq!(diagnostic.span.start, offset);
            assert_eq!(diagnostic.span.end, offset + marker.len());

            let recovered = parse(&format!("{source}\nCREATE SCHEMA recovered;"));
            assert_eq!(
                recovered.schemas().len(),
                1,
                "later declaration lost after: {source}"
            );
        }
    }

    #[test]
    fn field_rename_recovery_preserves_later_declarations() {
        let source = "ALTER TYPE people.person RENAME FIELD email TO;\n\
            CREATE TYPE people.person AS OBJECT (primary_email TEXT);\n\
            ALTER TYPE people.person RENAME FIELD email TO primary_email;";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.object_types().len(), 1);
        assert_eq!(parsed.field_renames().len(), 1);
        assert_eq!(
            parsed.field_renames()[0].new_field_name.text,
            "primary_email"
        );
        assert_eq!(parsed.diagnostics().len(), 1);
    }

    #[test]
    fn malformed_field_rename_quotes_use_the_existing_lexer_diagnostic() {
        let source = "ALTER TYPE people.person RENAME FIELD \"email TO primary_email;";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert!(parsed.field_renames().is_empty());
        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(parsed.diagnostics()[0].code, "ORNA0002");
        assert_eq!(
            parsed.diagnostics()[0].message,
            "unterminated quoted identifier"
        );
        assert_eq!(
            parsed.diagnostics()[0].span,
            SourceSpan {
                start: source.find('"').unwrap(),
                end: source.len(),
            }
        );
    }

    #[test]
    fn unsupported_top_level_statements_report_one_clear_error() {
        let source = "DROP TYPE people.person;";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert!(parsed.schemas().is_empty());
        assert!(parsed.object_types().is_empty());
        assert!(parsed.field_renames().is_empty());
        assert!(parsed.server_functions().is_empty());
        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(parsed.diagnostics()[0].code, "ORNA0001");
        assert_eq!(
            parsed.diagnostics()[0].message,
            "expected a CREATE, ALTER, or EXPORT declaration"
        );
        assert_eq!(
            parsed.diagnostics()[0].span,
            SourceSpan { start: 0, end: 4 }
        );
    }

    #[test]
    fn field_rename_parsing_does_not_change_create_or_select_parsing() {
        let source = "CREATE TYPE people.person AS OBJECT (primary_email TEXT);\n\
            ALTER TYPE people.person RENAME FIELD email TO primary_email;\n\
            CREATE SERVER FUNCTION people.list_emails() RETURNS ROWS (email TEXT) AS SELECT person.primary_email FROM people.person person;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.object_types().len(), 1);
        assert_eq!(parsed.field_renames().len(), 1);
        assert_eq!(parsed.server_functions().len(), 1);
        let query = parsed.server_functions()[0]
            .body
            .as_sql_query()
            .expect("function must retain its SELECT query");
        assert_eq!(query.query.projections.len(), 1);
        assert_eq!(query.query.source_object.alias.text, "person");
    }

    #[test]
    fn rejects_primary_keys_in_object_types_with_an_explanatory_diagnostic() {
        let source = "CREATE TYPE people.person AS OBJECT (id INT PRIMARY KEY);";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert!(parsed.object_types().is_empty());
        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(parsed.diagnostics()[0].code, "ORNA0001");
        assert!(
            parsed.diagnostics()[0]
                .message
                .contains("use UNIQUE NOT NULL for a business identity")
        );
        assert_eq!(
            parsed.diagnostics()[0].span.start,
            source.find("PRIMARY").expect("PRIMARY exists")
        );
    }

    fn assert_insert_diagnostic(
        body: &str,
        message: &str,
        marker: &str,
        span_offset: usize,
        span_length: usize,
    ) {
        assert_body_diagnostic(
            "p_title TEXT, p_done BOOL",
            "result REF tasks.task",
            body,
            message,
            marker,
            span_offset,
            span_length,
        );
    }

    fn assert_update_diagnostic(
        body: &str,
        message: &str,
        marker: &str,
        span_offset: usize,
        span_length: usize,
    ) {
        assert_body_diagnostic(
            "p_task REF tasks.task, p_title TEXT",
            "result REF tasks.task",
            body,
            message,
            marker,
            span_offset,
            span_length,
        );
    }

    fn assert_delete_diagnostic(
        body: &str,
        message: &str,
        marker: &str,
        span_offset: usize,
        span_length: usize,
    ) {
        assert_body_diagnostic(
            "p_task REF tasks.task",
            "deleted BOOL",
            body,
            message,
            marker,
            span_offset,
            span_length,
        );
    }

    #[test]
    fn parses_client_boolean_constants_losslessly_with_exact_spans() {
        let source = "CREATE CLIENT FUNCTION examples.enabled()\n\
            RETURNS BOOLEAN\n\
            RETURN TRUE;\n\
            CREATE cLiEnT fUnCtIoN \"Examples\".\"Disabled\"() RETURNS BOOL RETURN fAlSe;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        assert!(parsed.server_functions().is_empty());
        assert_eq!(parsed.client_functions().len(), 2);

        let enabled = &parsed.client_functions()[0];
        assert_eq!(enabled.name.parts[0].text, "examples");
        assert_eq!(enabled.parameters.len(), 0);
        assert_eq!(enabled.span.start, 0);
        assert_eq!(
            enabled.span.end,
            source.find(';').expect("first terminator") + 1
        );
        let enabled_literal = enabled.body.as_boolean_literal().expect("Boolean body");
        assert!(enabled_literal.0);
        assert_eq!(enabled_literal.1.text, "TRUE");
        let enabled_start = source.find("TRUE").expect("TRUE literal");
        assert_eq!(
            enabled_literal.1.span,
            SourceSpan {
                start: enabled_start,
                end: enabled_start + 4,
            }
        );

        let disabled = &parsed.client_functions()[1];
        assert_eq!(disabled.name.parts[0].text, "\"Examples\"");
        assert_eq!(disabled.name.parts[1].text, "\"Disabled\"");
        let disabled_literal = disabled.body.as_boolean_literal().expect("Boolean body");
        assert!(!disabled_literal.0);
        assert_eq!(disabled_literal.1.text, "fAlSe");
        let disabled_start = source.find("fAlSe").expect("FALSE literal");
        assert_eq!(
            disabled_literal.1.span,
            SourceSpan {
                start: disabled_start,
                end: disabled_start + 5,
            }
        );
    }

    #[test]
    fn parses_short_client_return_expressions_without_broadening_the_closed_surface() {
        let source = "CREATE CLIENT FUNCTION examples.ui() RETURNS UI RETURN std.ui.text('Example');\n\
            CREATE CLIENT FUNCTION examples.text() RETURNS TEXT RETURN 'ready';";
        let parsed = parse(source);

        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        assert_eq!(parsed.client_functions().len(), 2);
        let ui = &parsed.client_functions()[0];
        let ClientFunctionBody::ReturnExpression { expression } = &ui.body else {
            panic!("expected a short RETURN expression body");
        };
        assert!(matches!(
            expression,
            ClientExpression::Call { callee, arguments, .. }
                if callee.parts.iter().map(|part| part.text.as_str()).eq(["std", "ui", "text"])
                    && arguments.len() == 1
        ));
        let text = &parsed.client_functions()[1];
        assert!(matches!(
            text.body.as_expression(),
            Some(ClientExpression::StringLiteral { value, .. }) if value == "ready"
        ));
    }

    #[test]
    fn parses_short_client_return_await_with_exact_expression_spans() {
        let source = "CREATE CLIENT FUNCTION examples.awaited() RETURNS TEXT RETURN AWAIT std.data.resource();";
        let parsed = parse(source);

        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        assert_eq!(parsed.syntax().text(), source);
        let ClientFunctionBody::ReturnExpression { expression } =
            &parsed.client_functions()[0].body
        else {
            panic!("expected a short RETURN expression body");
        };
        let ClientExpression::Await {
            expression: awaited,
            span,
        } = expression
        else {
            panic!("expected an AWAIT expression");
        };
        let await_start = source.find("AWAIT").expect("AWAIT keyword");
        let expression_end = source.rfind(");").expect("resource call terminator") + 1;
        assert_eq!(
            span,
            &SourceSpan {
                start: await_start,
                end: expression_end,
            }
        );
        assert_eq!(&source[span.start..span.end], "AWAIT std.data.resource()");
        let ClientExpression::Call {
            callee,
            span: resource_span,
            ..
        } = awaited.as_ref()
        else {
            panic!("expected AWAIT to wrap a resource call expression");
        };
        let resource_start = source.find("std.data.resource").expect("resource callee");
        assert_eq!(
            resource_span,
            &SourceSpan {
                start: resource_start,
                end: expression_end,
            }
        );
        assert_eq!(
            &source[resource_span.start..resource_span.end],
            "std.data.resource()"
        );
        assert_eq!(
            callee.span,
            SourceSpan {
                start: resource_start,
                end: resource_start + "std.data.resource".len(),
            }
        );
    }

    #[test]
    fn parses_canonical_accepted_dogfood_fixtures_losslessly() {
        let fixtures = [
            (
                "client_function_dogfood.orna",
                include_str!("../../orna-server/tests/fixtures/client_function_dogfood.orna"),
            ),
            (
                "scalar_resource_dogfood.orna",
                include_str!("../../orna-server/tests/fixtures/scalar_resource_dogfood.orna"),
            ),
            (
                "stream_resource_dogfood.orna",
                include_str!("../../orna-server/tests/fixtures/stream_resource_dogfood.orna"),
            ),
            (
                "action_dogfood.orna",
                include_str!("../../orna-server/tests/fixtures/action_dogfood.orna"),
            ),
            (
                "client_inspector_dogfood.orna",
                include_str!("../../orna-server/tests/fixtures/client_inspector_dogfood.orna"),
            ),
            (
                "expression_client_dogfood.orna",
                include_str!("../../orna-server/tests/fixtures/expression_client_dogfood.orna"),
            ),
            (
                "server_function_dogfood.orna",
                include_str!("../../orna-server/tests/fixtures/server_function_dogfood.orna"),
            ),
            (
                "client_local_assignment_dogfood.orna",
                include_str!(
                    "../../orna-server/tests/fixtures/client_local_assignment_dogfood.orna"
                ),
            ),
        ];

        for (name, source) in fixtures {
            let parsed = parse(source);
            assert!(
                parsed.diagnostics().is_empty(),
                "{name}: {:?}",
                parsed.diagnostics()
            );
            assert_eq!(parsed.syntax().text(), source, "{name}");
        }
    }

    #[test]
    fn parses_accepted_client_fixture_losslessly_with_expression_and_state_bodies() {
        let source = include_str!("../testdata/accepted-client.orna");
        let parsed = parse(source);

        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.schemas().len(), 1);
        assert_eq!(parsed.schemas()[0].name.parts[0].text, "accepted_client");
        assert_eq!(parsed.client_functions().len(), 2);

        let expression = &parsed.client_functions()[0];
        assert_eq!(expression.name.parts[0].text, "accepted_client");
        assert_eq!(expression.name.parts[1].text, "enabled");
        let ClientFunctionBody::Expression { expression } = &expression.body else {
            panic!("expected an expression CLIENT body");
        };
        assert!(matches!(
            expression,
            ClientExpression::BooleanLiteral { value: true, .. }
        ));

        let stateful = &parsed.client_functions()[1];
        assert_eq!(stateful.name.parts[0].text, "accepted_client");
        assert_eq!(stateful.name.parts[1].text, "stateful");
        let ClientFunctionBody::StateBlock(block) = &stateful.body else {
            panic!("expected a state CLIENT body");
        };
        assert_eq!(block.states.len(), 1);
        assert!(block.locals.is_empty());
        assert!(block.statements.is_empty());
        let state = &block.states[0];
        assert_eq!(state.name.text, "ready");
        assert!(matches!(
            &state.type_specification,
            TypeSpecification::Named(name) if name.parts[0].text == "BOOLEAN"
        ));
        assert_eq!(state.scope, StateScope::Local);
        assert!(matches!(
            &state.default,
            StateDefault::Expression(ClientExpression::BooleanLiteral { value: true, .. })
        ));
        assert!(matches!(
            block.return_expression.as_ref(),
            Some(ClientExpression::BooleanLiteral { value: true, .. })
        ));
    }

    #[test]
    fn retains_client_parameters_and_non_boolean_return_types_for_semantic_checks() {
        let source = "CREATE CLIENT FUNCTION examples.with_parameter(p_value TEXT) RETURNS BOOLEAN RETURN TRUE;\n\
            CREATE CLIENT FUNCTION examples.ui() RETURNS UI RETURN FALSE;\n\
            CREATE CLIENT FUNCTION examples.text() RETURNS TEXT RETURN TRUE;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.client_functions().len(), 3);
        let with_parameter = &parsed.client_functions()[0];
        assert_eq!(with_parameter.parameters.len(), 1);
        assert_eq!(with_parameter.parameters[0].name.text, "p_value");
        assert_eq!(with_parameter.parameters[0].order, 0);
        assert_eq!(
            with_parameter.parameters[0].span.start,
            source.find("p_value").unwrap()
        );
        let parameter_list = "(p_value TEXT)";
        let parameter_list_start = source.find(parameter_list).unwrap();
        assert_eq!(
            with_parameter.parameter_list_span,
            SourceSpan {
                start: parameter_list_start,
                end: parameter_list_start + parameter_list.len(),
            }
        );
        let empty_parameter_list_start =
            source.find("examples.ui()").unwrap() + "examples.ui".len();
        assert_eq!(
            parsed.client_functions()[1].parameter_list_span,
            SourceSpan {
                start: empty_parameter_list_start,
                end: empty_parameter_list_start + 2,
            }
        );
        assert!(matches!(
            &parsed.client_functions()[1].return_type,
            FunctionReturnType::Single(TypeSpecification::Named(name))
                if name.parts[0].text == "UI"
        ));
        assert!(matches!(
            &parsed.client_functions()[2].return_type,
            FunctionReturnType::Single(TypeSpecification::Named(name))
                if name.parts[0].text == "TEXT"
        ));
    }

    #[test]
    fn reports_closed_client_body_diagnostics_with_exact_public_messages() {
        let cases = [
            (
                "CREATE CLIENT FUNCTION examples.security() RETURNS BOOLEAN SECURITY INVOKER RETURN TRUE;",
                "CLIENT functions use RETURN before their result value",
                "SECURITY",
            ),
            (
                "CREATE CLIENT FUNCTION examples.transaction() RETURNS BOOLEAN TRANSACTION READ ONLY RETURN TRUE;",
                "CLIENT functions use RETURN before their result value",
                "TRANSACTION",
            ),
            (
                "CREATE CLIENT FUNCTION examples.volatility() RETURNS BOOLEAN VOLATILITY IMMUTABLE RETURN TRUE;",
                "CLIENT functions use RETURN before their result value",
                "VOLATILITY",
            ),
            (
                "CREATE CLIENT FUNCTION examples.table_result() RETURNS TABLE (value BOOLEAN) RETURN TRUE;",
                "CLIENT functions must name one return type after RETURNS",
                "TABLE",
            ),
            (
                "CREATE CLIENT FUNCTION examples.set_result() RETURNS SET OF BOOLEAN RETURN TRUE;",
                "CLIENT functions must name one return type after RETURNS",
                "SET",
            ),
            (
                "CREATE CLIENT FUNCTION examples.missing_type() RETURNS ;",
                "CLIENT functions must name one return type after RETURNS",
                ";",
            ),
            (
                "CREATE CLIENT FUNCTION examples.extra() RETURNS BOOLEAN RETURN TRUE FALSE;",
                "expected ';' after CLIENT function body",
                "FALSE",
            ),
            (
                "CREATE CLIENT FUNCTION examples.missing_semicolon() RETURNS BOOLEAN RETURN TRUE",
                "expected ';' after CLIENT function body",
                "",
            ),
        ];

        for (source, message, marker) in cases {
            let parsed = parse(source);
            assert!(parsed.client_functions().is_empty(), "source: {source}");
            assert_eq!(parsed.diagnostics().len(), 1, "source: {source}");
            let diagnostic = &parsed.diagnostics()[0];
            assert_eq!(diagnostic.code, "ORNA0001");
            assert_eq!(diagnostic.message, message);
            if marker.is_empty() {
                assert_eq!(diagnostic.span.start, source.len());
                assert_eq!(diagnostic.span.end, source.len());
            } else {
                let start = source.find(marker).expect("diagnostic marker");
                assert_eq!(diagnostic.span.start, start);
                assert_eq!(diagnostic.span.end, start + marker.len());
            }
        }
    }

    #[test]
    fn parses_client_expression_bodies_and_external_contracts_with_exact_spans() {
        let source = "CREATE CLIENT FUNCTION examples.greeting(p_name TEXT)\n\
            RETURNS TEXT\n\
            AS std.strings.concat('Hello ', p_name);\n\
            CREATE CLIENT FUNCTION examples.qualified() RETURNS BOOLEAN AS TRUE;\n\
            CREATE EXTERNAL CLIENT FUNCTION std.ui.window (\n\
                title TEXT,\n\
                content std.ui.UI\n\
            )\n\
            RETURNS std.ui.UI\n\
            RUNTIME CONTRACT 'std.ui.window@1';";
        let parsed = parse(source);

        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.client_functions().len(), 3);

        let greeting = &parsed.client_functions()[0];
        assert!(!greeting.external);
        assert_eq!(greeting.runtime_contract, None);
        assert_eq!(greeting.parameters.len(), 1);
        let ClientFunctionBody::Expression { expression } = &greeting.body else {
            panic!("expected an expression body");
        };
        let ClientExpression::Call {
            callee,
            arguments,
            span,
        } = expression
        else {
            panic!("expected a call expression");
        };
        assert_eq!(callee.parts.len(), 3);
        assert_eq!(callee.parts[0].text, "std");
        assert_eq!(callee.parts[2].text, "concat");
        assert_eq!(arguments.len(), 2);
        assert_eq!(arguments[0].name, None);
        let ClientExpression::StringLiteral { value, .. } = &arguments[0].value else {
            panic!("expected a string literal argument");
        };
        assert_eq!(value, "Hello ");
        assert_eq!(
            arguments[1].name.as_ref().map(|name| name.text.as_str()),
            None
        );
        let ClientExpression::ParameterRead { parameter } = &arguments[1].value else {
            panic!("expected a parameter read argument");
        };
        assert_eq!(parameter.text, "p_name");
        assert_eq!(span.start, source.find("std.strings").expect("callee"));

        let qualified = &parsed.client_functions()[1];
        let ClientFunctionBody::Expression { expression } = &qualified.body else {
            panic!("expected an expression body");
        };
        let ClientExpression::BooleanLiteral { value, .. } = expression else {
            panic!("expected a boolean literal expression");
        };
        assert!(*value);

        let external = &parsed.client_functions()[2];
        assert!(external.external);
        let contract = external
            .runtime_contract
            .as_ref()
            .expect("external functions carry a contract");
        assert_eq!(contract.text, "'std.ui.window@1'");
        let contract_start = source.find("'std.ui.window@1'").expect("contract");
        assert_eq!(
            contract.span,
            SourceSpan {
                start: contract_start,
                end: contract_start + "'std.ui.window@1'".len(),
            }
        );
        let ClientFunctionBody::ExternalContract { identity } = &external.body else {
            panic!("expected an external-contract body");
        };
        assert_eq!(identity.text, "'std.ui.window@1'");
    }

    #[test]
    fn parses_external_contract_with_capability_clause_in_source_order() {
        let source = "CREATE EXTERNAL CLIENT FUNCTION std.net.connect (\n\
            p_host TEXT\n\
        )\n\
        RETURNS BOOLEAN\n\
        RUNTIME CONTRACT 'std.net.connect@1'\n\
        REQUIRES CAPABILITY std.net.connect(p_host);";
        let parsed = parse(source);

        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        assert_eq!(parsed.syntax().text(), source);
        let function = &parsed.client_functions()[0];
        assert!(function.external);
        assert_eq!(function.capabilities.len(), 1);
        let contract = function
            .runtime_contract
            .as_ref()
            .expect("external functions carry a contract");
        assert_eq!(contract.text, "'std.net.connect@1'");
        let ClientFunctionBody::ExternalContract { identity } = &function.body else {
            panic!("expected an external-contract body");
        };
        assert_eq!(identity.text, "'std.net.connect@1'");
    }

    #[test]
    fn parses_client_concat_and_field_path_expressions() {
        let source = "CREATE CLIENT FUNCTION examples.label(p_item REF app.item)\n\
            RETURNS TEXT\n\
            AS p_item.name || ' #' || p_item.code;";
        let parsed = parse(source);

        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        assert_eq!(parsed.syntax().text(), source);
        let function = &parsed.client_functions()[0];
        let ClientFunctionBody::Expression { expression } = &function.body else {
            panic!("expected an expression body");
        };
        let ClientExpression::Concat {
            left: outer_left,
            right: outer_right,
            ..
        } = expression
        else {
            panic!("expected a concatenation");
        };
        let ClientExpression::Concat { left, right, .. } = outer_left.as_ref() else {
            panic!("expected a left-nested concatenation");
        };
        let ClientExpression::FieldPath { root, members, .. } = left.as_ref() else {
            panic!("expected a field path on the left");
        };
        assert_eq!(root.text, "p_item");
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].text, "name");
        let ClientExpression::StringLiteral { value, .. } = right.as_ref() else {
            panic!("expected the literal in the middle");
        };
        assert_eq!(value, " #");
        let ClientExpression::FieldPath { root, members, .. } = outer_right.as_ref() else {
            panic!("expected a field path on the right");
        };
        assert_eq!(root.text, "p_item");
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].text, "code");
    }

    #[test]
    fn parses_client_action_call_with_named_target_arguments_and_exact_spans() {
        let source = "CREATE CLIENT FUNCTION app.owner() RETURNS std.Action AS\n\
            std.action.call(\n\
                target => std.invoke.echo,\n\
                arguments => std.call.args(p_value => app.first())\n\
            );";
        let parsed = parse(source);

        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        let ClientFunctionBody::Expression { expression } = &parsed.client_functions()[0].body
        else {
            panic!("expected an expression body");
        };
        let ClientExpression::Call {
            callee,
            arguments,
            span,
        } = expression
        else {
            panic!("expected std.action.call expression");
        };

        let action_start = source.find("std.action.call").expect("action callee");
        let action_end = source.rfind(')').expect("action closing parenthesis") + 1;
        assert_eq!(
            callee
                .parts
                .iter()
                .map(|part| part.text.as_str())
                .collect::<Vec<_>>(),
            ["std", "action", "call"]
        );
        assert_eq!(
            callee.span,
            SourceSpan {
                start: action_start,
                end: action_start + "std.action.call".len(),
            }
        );
        assert_eq!(
            span,
            &SourceSpan {
                start: action_start,
                end: action_end
            }
        );
        assert_eq!(arguments.len(), 2);

        let target_start = source.find("target").expect("target argument");
        let target_value_start = source.find("std.invoke.echo").expect("target value");
        let target_argument = &arguments[0];
        let target_name = target_argument.name.as_ref().expect("named target");
        assert_eq!(target_name.text, "target");
        assert_eq!(
            target_name.span,
            SourceSpan {
                start: target_start,
                end: target_start + "target".len(),
            }
        );
        assert_eq!(
            target_argument.span,
            SourceSpan {
                start: target_start,
                end: target_value_start + "std.invoke.echo".len(),
            }
        );
        let ClientExpression::FieldPath {
            root,
            members,
            span: target_span,
        } = &target_argument.value
        else {
            panic!("expected a qualified target");
        };
        assert_eq!(target_span.start, target_value_start);
        assert_eq!(
            target_span.end,
            target_value_start + "std.invoke.echo".len()
        );
        assert_eq!(root.text, "std");
        assert_eq!(
            members
                .iter()
                .map(|member| member.text.as_str())
                .collect::<Vec<_>>(),
            ["invoke", "echo"]
        );

        let arguments_start = source.find("arguments").expect("arguments argument");
        let nested_start = source
            .find("std.call.args")
            .expect("nested arguments callee");
        let nested_end = source[nested_start..]
            .find("))")
            .expect("nested arguments closing parenthesis")
            + nested_start
            + 2;
        let arguments_argument = &arguments[1];
        let arguments_name = arguments_argument.name.as_ref().expect("named arguments");
        assert_eq!(arguments_name.text, "arguments");
        assert_eq!(
            arguments_name.span,
            SourceSpan {
                start: arguments_start,
                end: arguments_start + "arguments".len(),
            }
        );
        assert_eq!(
            arguments_argument.span,
            SourceSpan {
                start: arguments_start,
                end: nested_end,
            }
        );
        let ClientExpression::Call {
            callee: nested_callee,
            arguments: nested_arguments,
            span: nested_span,
        } = &arguments_argument.value
        else {
            panic!("expected std.call.args expression");
        };
        assert_eq!(
            nested_callee
                .parts
                .iter()
                .map(|part| part.text.as_str())
                .collect::<Vec<_>>(),
            ["std", "call", "args"]
        );
        assert_eq!(
            nested_callee.span,
            SourceSpan {
                start: nested_start,
                end: nested_start + "std.call.args".len(),
            }
        );
        assert_eq!(
            nested_span,
            &SourceSpan {
                start: nested_start,
                end: nested_end
            }
        );
        assert_eq!(nested_arguments.len(), 1);

        let pair_start = source
            .find("p_value => app.first()")
            .expect("nested argument");
        let pair_value_start = pair_start + "p_value => ".len();
        let pair_value_end = pair_start + "p_value => app.first()".len();
        let nested_argument = &nested_arguments[0];
        let nested_name = nested_argument
            .name
            .as_ref()
            .expect("named nested argument");
        assert_eq!(nested_name.text, "p_value");
        assert_eq!(
            nested_name.span,
            SourceSpan {
                start: pair_start,
                end: pair_start + "p_value".len(),
            }
        );
        assert_eq!(
            nested_argument.span,
            SourceSpan {
                start: pair_start,
                end: pair_value_end,
            }
        );
        let ClientExpression::Call {
            callee: target_call_callee,
            span: target_call_span,
            ..
        } = &nested_argument.value
        else {
            panic!("expected nested target argument call");
        };
        assert_eq!(
            target_call_callee
                .parts
                .iter()
                .map(|part| part.text.as_str())
                .collect::<Vec<_>>(),
            ["app", "first"]
        );
        assert_eq!(
            target_call_span,
            &SourceSpan {
                start: pair_value_start,
                end: pair_value_end,
            }
        );
    }

    #[test]
    fn parses_client_await_expression_losslessly_with_complete_span() {
        let source = "CREATE CLIENT FUNCTION examples.awaited(p_value TEXT) RETURNS TEXT IS\n\
            BEGIN\n\
                RETURN AWAIT /* preserve */ std.data.resource(\n\
                    target => tasks.get,\n\
                    arguments => std.call.args(p_value => p_value)\n\
                );\n\
            END;";
        let parsed = parse(source);

        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(
            parsed
                .syntax()
                .root()
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::ClientAwaitExpression)
                .count(),
            1
        );
        let ClientFunctionBody::StateBlock(block) = &parsed.client_functions()[0].body else {
            panic!("expected a procedural body");
        };
        let expression = block
            .return_expression
            .as_ref()
            .expect("expected a return expression");
        let ClientExpression::Await {
            expression: awaited,
            span,
        } = expression
        else {
            panic!("expected an AWAIT expression");
        };
        let expected_start = source.find("AWAIT").expect("AWAIT keyword");
        let expected_end = source[expected_start..]
            .find(");")
            .expect("resource statement terminator")
            + expected_start
            + 1;
        assert_eq!(
            span,
            &SourceSpan {
                start: expected_start,
                end: expected_end,
            }
        );
        let resource_start = source.find("std.data.resource").expect("resource callee");
        let resource_end = source.rfind(')').expect("resource closing parenthesis") + 1;
        let ClientExpression::Call {
            callee: resource_callee,
            arguments: resource_arguments,
            span: resource_span,
        } = awaited.as_ref()
        else {
            panic!("expected AWAIT to wrap a resource call expression");
        };
        assert_eq!(
            resource_span,
            &SourceSpan {
                start: resource_start,
                end: resource_end,
            }
        );
        assert_eq!(
            resource_callee.span,
            SourceSpan {
                start: resource_start,
                end: resource_start + "std.data.resource".len(),
            }
        );
        assert_eq!(
            resource_callee
                .parts
                .iter()
                .map(|part| part.text.as_str())
                .collect::<Vec<_>>(),
            ["std", "data", "resource"]
        );

        let target_start = source.find("target").expect("target argument");
        let target_name = resource_arguments[0]
            .name
            .as_ref()
            .expect("named target argument");
        assert_eq!(target_name.text, "target");
        assert_eq!(
            target_name.span,
            SourceSpan {
                start: target_start,
                end: target_start + "target".len(),
            }
        );
        let target_value_start = source.find("tasks.get").expect("resource target");
        assert_eq!(
            resource_arguments[0].span,
            SourceSpan {
                start: target_start,
                end: target_value_start + "tasks.get".len(),
            }
        );
        let ClientExpression::FieldPath {
            root: target_root,
            members: target_members,
            span: target_span,
        } = &resource_arguments[0].value
        else {
            panic!("expected a qualified target name");
        };
        assert_eq!(
            target_span,
            &SourceSpan {
                start: target_value_start,
                end: target_value_start + "tasks.get".len(),
            }
        );
        assert_eq!(target_root.text, "tasks");
        assert_eq!(
            target_root.span,
            SourceSpan {
                start: target_value_start,
                end: target_value_start + "tasks".len(),
            }
        );
        assert_eq!(target_members.len(), 1);
        assert_eq!(target_members[0].text, "get");
        assert_eq!(
            target_members[0].span,
            SourceSpan {
                start: target_value_start + "tasks.".len(),
                end: target_value_start + "tasks.get".len(),
            }
        );

        let arguments_start = source.find("arguments").expect("arguments argument");
        let arguments_name = resource_arguments[1]
            .name
            .as_ref()
            .expect("named arguments argument");
        assert_eq!(arguments_name.text, "arguments");
        assert_eq!(
            arguments_name.span,
            SourceSpan {
                start: arguments_start,
                end: arguments_start + "arguments".len(),
            }
        );
        let nested_start = source.find("std.call.args").expect("arguments call");
        let nested_end = source[nested_start..]
            .find(')')
            .expect("arguments closing parenthesis")
            + nested_start
            + 1;
        assert_eq!(
            resource_arguments[1].span,
            SourceSpan {
                start: arguments_start,
                end: nested_end,
            }
        );
        let ClientExpression::Call {
            callee: arguments_callee,
            arguments: nested_arguments,
            span: arguments_span,
        } = &resource_arguments[1].value
        else {
            panic!("expected std.call.args expression");
        };
        assert_eq!(
            arguments_span,
            &SourceSpan {
                start: nested_start,
                end: nested_end,
            }
        );
        assert_eq!(
            arguments_callee.span,
            SourceSpan {
                start: nested_start,
                end: nested_start + "std.call.args".len(),
            }
        );
        assert_eq!(
            arguments_callee
                .parts
                .iter()
                .map(|part| part.text.as_str())
                .collect::<Vec<_>>(),
            ["std", "call", "args"]
        );
        assert_eq!(nested_arguments.len(), 1);

        let nested_argument = &nested_arguments[0];
        let pair_start = source
            .find("p_value => p_value")
            .expect("nested named argument");
        let pair_value_start = pair_start + "p_value => ".len();
        let pair_value_end = pair_start + "p_value => p_value".len();
        let pair_name = nested_argument.name.as_ref().expect("nested argument name");
        assert_eq!(pair_name.text, "p_value");
        assert_eq!(
            pair_name.span,
            SourceSpan {
                start: pair_start,
                end: pair_start + "p_value".len(),
            }
        );
        assert_eq!(
            nested_argument.span,
            SourceSpan {
                start: pair_start,
                end: pair_value_end,
            }
        );
        let ClientExpression::ParameterRead { parameter } = &nested_argument.value else {
            panic!("expected the nested argument value to read p_value");
        };
        assert_eq!(parameter.text, "p_value");
        assert_eq!(
            parameter.span,
            SourceSpan {
                start: pair_value_start,
                end: pair_value_end,
            }
        );
    }
    #[test]
    fn rejects_client_await_in_state_declaration_positions() {
        let source = "CREATE CLIENT FUNCTION examples.awaited() RETURNS TEXT IS\n\
            STATE value TEXT DEFAULT AWAIT std.data.resource();\n\
        BEGIN\n\
            RETURN value;\n\
        END;";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert!(parsed.client_functions().is_empty());
        assert!(
            parsed
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code == "ORNA0001"),
            "expected an ORNA0001 diagnostic, got {:?}",
            parsed.diagnostics()
        );
    }

    #[test]
    fn parses_client_local_resource_binding_and_await_return() {
        let source = "CREATE CLIENT FUNCTION studio.overdue_rows(p_owner REF studio.owner)\n\
            RETURNS TEXT IS\n\
            LET rows std.data.Resource<TABLE(task_id UUID, title TEXT)> :=\n\
                std.data.resource(\n\
                    target => tasks.overdue,\n\
                    arguments => std.call.args(p_owner => p_owner)\n\
                );\n\
            BEGIN\n\
                RETURN AWAIT rows;\n\
            END;";
        let parsed = parse(source);

        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(
            parsed
                .syntax()
                .root()
                .descendants()
                .filter(|node| node.kind() == SyntaxKind::ClientLocalBinding)
                .count(),
            1
        );
        let ClientFunctionBody::StateBlock(block) = &parsed.client_functions()[0].body else {
            panic!("expected a procedural CLIENT block");
        };
        assert!(block.states.is_empty());
        assert_eq!(block.locals.len(), 1);
        let local = &block.locals[0];
        assert_eq!(local.name.text, "rows");
        assert_eq!(
            local.type_source.text,
            "std.data.Resource<TABLE(task_id UUID, title TEXT)>"
        );
        assert_eq!(
            local.type_source.span,
            SourceSpan {
                start: source.find("std.data.Resource").expect("local type"),
                end: source[..source.find(":=").expect("initializer marker")]
                    .trim_end()
                    .len(),
            }
        );
        let ClientExpression::Call { callee, .. } = &local.expression else {
            panic!("expected a resource constructor call");
        };
        assert_eq!(
            callee
                .parts
                .iter()
                .map(|part| part.text.as_str())
                .collect::<Vec<_>>(),
            ["std", "data", "resource"]
        );
        let Some(ClientExpression::Await { expression, .. }) = block.return_expression.as_ref()
        else {
            panic!("expected an AWAIT return expression");
        };
        assert!(matches!(
            expression.as_ref(),
            ClientExpression::LocalRead { local } if local.text == "rows"
        ));
    }

    #[test]
    fn parses_post_begin_client_procedural_statements_losslessly() {
        let source = "CREATE CLIENT FUNCTION examples.procedural() RETURNS INTEGER IS\n\
            BEGIN\n\
                LET x std.data.Resource<INTEGER> := AWAIT std.data.resource();\n\
                x := AWAIT std.data.resource();\n\
                RETURN x;\n\
            END;";
        let parsed = parse(source);

        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        assert_eq!(parsed.syntax().text(), source);
        let ClientFunctionBody::StateBlock(block) = &parsed.client_functions()[0].body else {
            panic!("expected a procedural CLIENT block");
        };
        assert!(block.states.is_empty());
        assert!(block.locals.is_empty());
        assert_eq!(block.statements.len(), 2);
        let ClientProceduralStatement::Let(let_statement) = &block.statements[0] else {
            panic!("expected a procedural LET statement");
        };
        assert_eq!(let_statement.name.text, "x");
        assert_eq!(
            let_statement
                .type_source
                .as_ref()
                .map(|source| source.text.as_str()),
            Some("std.data.Resource<INTEGER>")
        );
        assert!(matches!(
            let_statement.expression,
            ClientExpression::Await { .. }
        ));
        let ClientProceduralStatement::Assignment(assignment) = &block.statements[1] else {
            panic!("expected a procedural assignment statement");
        };
        assert_eq!(assignment.target.text, "x");
        assert!(matches!(
            assignment.expression,
            ClientExpression::Await { .. }
        ));
        assert!(matches!(
            block.return_expression,
            Some(ClientExpression::LocalRead { .. })
        ));
        assert_eq!(
            &source[let_statement.span.start..let_statement.span.end],
            "LET x std.data.Resource<INTEGER> := AWAIT std.data.resource();"
        );
        assert_eq!(
            &source[assignment.span.start..assignment.span.end],
            "x := AWAIT std.data.resource();"
        );
    }

    #[test]
    fn parses_untyped_post_begin_await_let_and_local_read_return() {
        let source = "CREATE CLIENT FUNCTION examples.untyped_procedural() RETURNS INTEGER IS\n\
            BEGIN\n\
                LET value := AWAIT std.data.resource();\n\
                RETURN value;\n\
            END;";
        let parsed = parse(source);

        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        assert_eq!(parsed.syntax().text(), source);
        let ClientFunctionBody::StateBlock(block) = &parsed.client_functions()[0].body else {
            panic!("expected a procedural CLIENT block");
        };
        assert_eq!(block.statements.len(), 1);
        let ClientProceduralStatement::Let(let_statement) = &block.statements[0] else {
            panic!("expected a procedural LET statement");
        };
        assert_eq!(let_statement.name.text, "value");
        assert!(let_statement.type_source.is_none());
        assert!(matches!(
            let_statement.expression,
            ClientExpression::Await { .. }
        ));
        assert!(matches!(
            &block.return_expression,
            Some(ClientExpression::LocalRead { local }) if local.text == "value"
        ));
    }

    #[test]
    fn rejects_client_await_in_expression_bodies() {
        let source = "CREATE CLIENT FUNCTION examples.awaited() RETURNS TEXT AS\n\
            AWAIT std.data.resource();";
        let parsed = parse(source);

        assert_eq!(parsed.syntax().text(), source);
        assert!(parsed.client_functions().is_empty());
        assert_eq!(parsed.diagnostics().len(), 1, "{:?}", parsed.diagnostics());
        let diagnostic = &parsed.diagnostics()[0];
        assert_eq!(diagnostic.code, "ORNA0001");
        let await_start = source.find("AWAIT").expect("AWAIT keyword");
        assert_eq!(
            diagnostic.span,
            SourceSpan {
                start: await_start,
                end: await_start + "AWAIT".len(),
            }
        );
    }

    #[test]
    fn reports_malformed_client_await_operands_without_widening_expression_syntax() {
        for expression in ["AWAIT;", "AWAIT (value);", "AWAIT AWAIT;"] {
            let source =
                format!("CREATE CLIENT FUNCTION examples.awaited() RETURNS TEXT AS {expression}");
            let parsed = parse(&source);

            assert_eq!(parsed.syntax().text(), source);
            assert!(parsed.client_functions().is_empty(), "{expression:?}");
            assert!(
                parsed
                    .diagnostics()
                    .iter()
                    .any(|diagnostic| diagnostic.message == "expected a CLIENT expression"),
                "{expression:?}: {:?}",
                parsed.diagnostics()
            );
        }
    }

    #[test]
    fn rejects_client_expression_trailing_dots() {
        for expression in ["p.", "p_item.name."] {
            let source = format!(
                "CREATE CLIENT FUNCTION examples.read(p_item REF app.item) \
                 RETURNS TEXT AS {expression};"
            );
            let parsed = parse(&source);

            assert!(
                parsed
                    .diagnostics()
                    .iter()
                    .any(|diagnostic| diagnostic.message
                        == "expected an identifier after a CLIENT expression dot"),
                "{expression:?}: {:?}",
                parsed.diagnostics()
            );
        }
    }

    #[test]
    fn parses_client_function_capability_clauses_with_exact_names_arguments_and_spans() {
        let source = "CREATE CLIENT FUNCTION examples.hash_file(p_file std.fs.Path)\n\
            RETURNS BYTES\n\
            REQUIRES CAPABILITY std.fs.read(p_file), std.fs.write(p_file), std.net.call, std.secret.use()\n\
            RETURN TRUE;\n\
            CREATE CLIENT FUNCTION examples.bare() RETURNS BOOLEAN RETURN FALSE;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.client_functions().len(), 2);

        let hash_file = &parsed.client_functions()[0];
        let capabilities = &hash_file.capabilities;
        assert_eq!(capabilities.len(), 4);
        assert_eq!(capabilities[0].name.parts[0].text, "std");
        assert_eq!(capabilities[0].name.parts[1].text, "fs");
        assert_eq!(capabilities[0].name.parts[2].text, "read");
        assert_eq!(
            capabilities[0]
                .arguments
                .as_ref()
                .map(|arguments| arguments.text.as_str()),
            Some("p_file"),
        );
        let read_clause = "std.fs.read(p_file)";
        let read_clause_start = source.find(read_clause).expect("read clause");
        assert_eq!(
            capabilities[0].span,
            SourceSpan {
                start: read_clause_start,
                end: read_clause_start + read_clause.len(),
            }
        );
        let read_arguments = capabilities[0].arguments.as_ref().expect("read arguments");
        assert_eq!(
            read_arguments.span,
            SourceSpan {
                start: read_clause_start + "std.fs.read(".len(),
                end: read_clause_start + "std.fs.read(p_file".len(),
            }
        );
        assert_eq!(
            capabilities[1]
                .arguments
                .as_ref()
                .map(|arguments| arguments.text.as_str()),
            Some("p_file"),
        );
        assert!(capabilities[2].arguments.is_none());
        assert_eq!(
            capabilities[3]
                .arguments
                .as_ref()
                .map(|arguments| arguments.text.as_str()),
            Some(""),
        );
        let bare = &parsed.client_functions()[1];
        assert!(bare.capabilities.is_empty());
    }

    #[test]
    fn rejects_malformed_client_function_capability_clauses() {
        let cases = [
            (
                "CREATE CLIENT FUNCTION examples.bad() RETURNS BOOLEAN REQUIRES CAPABILITY RETURN TRUE;",
                "expected a capability after REQUIRES CAPABILITY",
                "RETURN",
            ),
            (
                "CREATE CLIENT FUNCTION examples.bad() RETURNS BOOLEAN REQUIRES CAPABILITY std.fs.read, RETURN TRUE;",
                "trailing commas are not allowed in capability requirements",
                "RETURN",
            ),
            (
                "CREATE CLIENT FUNCTION examples.bad() RETURNS BOOLEAN REQUIRES CAPABILITY std.fs.read REQUIRES CAPABILITY std.fs.write RETURN TRUE;",
                "expected ',' or a body keyword after a capability requirement",
                "REQUIRES",
            ),
            (
                "CREATE CLIENT FUNCTION examples.bad() RETURNS BOOLEAN REQUIRES CAPABILITY std.fs.read(p_file RETURN TRUE;",
                "expected ')' to close capability arguments",
                ";",
            ),
            (
                "CREATE CLIENT FUNCTION examples.is_form() RETURNS BOOLEAN REQUIRES CAPABILITY std.fs.read(p_file);",
                "CLIENT functions use RETURN before their result value",
                ";",
            ),
        ];

        for (source, message, marker) in cases {
            let parsed = parse(source);
            assert!(parsed.client_functions().is_empty(), "source: {source}");
            assert_eq!(parsed.diagnostics().len(), 1, "source: {source}");
            let diagnostic = &parsed.diagnostics()[0];
            assert_eq!(diagnostic.code, "ORNA0001");
            assert_eq!(diagnostic.message, message);
            let start = source
                .match_indices(marker)
                .last()
                .map(|(index, _)| index)
                .expect("diagnostic marker");
            assert_eq!(diagnostic.span.start, start);
            assert_eq!(diagnostic.span.end, start + marker.len());
        }
    }

    #[test]
    fn recovers_after_client_function_errors_to_all_later_declarations() {
        let source = "CREATE CLIENT FUNCTION examples.bad() RETURNS BOOLEAN SECURITY INVOKER RETURN TRUE;\n\
            CREATE SCHEMA later;\n\
            CREATE TYPE later.item AS OBJECT (name TEXT);\n\
            CREATE SERVER FUNCTION later.server() RETURNS ROWS (value BOOL) AS SELECT t.value FROM later.item t;\n\
            CREATE CLIENT FUNCTION later.good() RETURNS BOOLEAN RETURN FALSE;";
        let parsed = parse(source);

        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(
            parsed.diagnostics()[0].message,
            "CLIENT functions use RETURN before their result value"
        );
        let rejected_form = source.find("SECURITY").expect("rejected CLIENT body form");
        assert_eq!(
            parsed.diagnostics()[0].span,
            SourceSpan {
                start: rejected_form,
                end: rejected_form + 8,
            }
        );
        assert_eq!(parsed.schemas().len(), 1);
        assert_eq!(parsed.object_types().len(), 1);
        assert_eq!(parsed.server_functions().len(), 1);
        assert_eq!(parsed.client_functions().len(), 1);
        assert_eq!(parsed.client_functions()[0].name.parts[0].text, "later");
        assert_eq!(parsed.client_functions()[0].name.parts[1].text, "good");
    }

    #[test]
    fn parses_client_state_blocks_with_scopes_defaults_and_single_return() {
        let source = "CREATE CLIENT FUNCTION studio.connections()\n\
            RETURNS TEXT\n\
            IS\n\
                STATE filter TEXT SCOPE LOCAL DEFAULT '';\n\
                STATE selected TEXT SCOPE SESSION DEFAULT NULL;\n\
                STATE count INTEGER SCOPE USER;\n\
            BEGIN\n\
                RETURN filter || selected;\n\
            END;";
        let parsed = parse(source);

        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.client_functions().len(), 1);
        let function = &parsed.client_functions()[0];
        let ClientFunctionBody::StateBlock(block) = &function.body else {
            panic!("expected a state block body");
        };
        assert_eq!(block.states.len(), 3);

        let filter = &block.states[0];
        assert_eq!(filter.name.text, "filter");
        assert_eq!(filter.scope, StateScope::Local);
        assert!(matches!(
            filter.default,
            StateDefault::Expression(ClientExpression::StringLiteral { .. })
        ));
        assert!(matches!(
            &filter.type_specification,
            TypeSpecification::Named(name) if name.parts[0].text == "TEXT"
        ));

        let selected = &block.states[1];
        assert_eq!(selected.name.text, "selected");
        assert_eq!(selected.scope, StateScope::Session);
        // `DEFAULT NULL` represents an explicit null initial value.
        assert!(matches!(selected.default, StateDefault::Null));

        let count = &block.states[2];
        assert_eq!(count.name.text, "count");
        assert_eq!(count.scope, StateScope::User);
        assert!(matches!(count.default, StateDefault::Unset));

        let ClientExpression::Concat { .. } =
            block.return_expression.as_ref().expect("return expression")
        else {
            panic!("expected a concatenation return expression");
        };

        let filter_start = source.find("STATE filter").expect("filter declaration");
        let filter_end = source.find("'';").expect("filter terminator") + "'';".len();
        assert_eq!(
            filter.span,
            SourceSpan {
                start: filter_start,
                end: filter_end,
            }
        );
        let block_start = source.find("IS").expect("IS keyword");
        let block_end = source.find("END").expect("END keyword") + "END".len();
        assert_eq!(
            block.span,
            SourceSpan {
                start: block_start,
                end: block_end,
            }
        );
    }

    #[test]
    fn parses_client_state_blocks_with_bare_return_and_omitted_clauses() {
        let source = "CREATE CLIENT FUNCTION examples.reset() RETURNS BOOLEAN IS BEGIN RETURN; END;\n\
            CREATE CLIENT FUNCTION examples.touched() RETURNS TEXT IS\n\
                STATE stamp TEXT;\n\
            BEGIN\n\
                RETURN stamp;\n\
            END;";
        let parsed = parse(source);

        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(parsed.client_functions().len(), 2);

        let reset = &parsed.client_functions()[0];
        let ClientFunctionBody::StateBlock(block) = &reset.body else {
            panic!("expected a state block body");
        };
        assert!(block.states.is_empty());
        assert!(block.return_expression.is_none());

        let touched = &parsed.client_functions()[1];
        let ClientFunctionBody::StateBlock(block) = &touched.body else {
            panic!("expected a state block body");
        };
        assert_eq!(block.states.len(), 1);
        let stamp = &block.states[0];
        assert_eq!(stamp.name.text, "stamp");
        assert_eq!(stamp.scope, StateScope::Local);
        assert!(matches!(stamp.default, StateDefault::Unset));
        let ClientExpression::ParameterRead { parameter } =
            block.return_expression.as_ref().expect("return expression")
        else {
            panic!("expected a parameter read return expression");
        };
        assert_eq!(parameter.text, "stamp");
    }

    #[test]
    fn keeps_duplicate_state_names_for_the_compiler_to_reject() {
        let source = "CREATE CLIENT FUNCTION examples.dup() RETURNS TEXT IS\n\
            STATE stamp TEXT;\n\
            STATE stamp TEXT;\n\
        BEGIN\n\
            RETURN stamp;\n\
        END;";
        let parsed = parse(source);

        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        let ClientFunctionBody::StateBlock(block) = &parsed.client_functions()[0].body else {
            panic!("expected a state block body");
        };
        assert_eq!(block.states.len(), 2);
        assert_eq!(block.states[0].name.text, "stamp");
        assert_eq!(block.states[1].name.text, "stamp");
    }

    #[test]
    fn rejects_malformed_and_unsupported_procedural_statements_in_client_state_blocks() {
        let cases = [
            (
                "CREATE CLIENT FUNCTION examples.bad() RETURNS BOOLEAN IS LET x = 1; BEGIN RETURN TRUE; END;",
                "CLIENT local bindings require a declared type and ':=' initializer",
                "=",
            ),
            (
                "CREATE CLIENT FUNCTION examples.bad() RETURNS BOOLEAN IS STATE count TEXT; BEGIN RETURN 1; RETURN 2; END;",
                "CLIENT blocks accept only a single RETURN statement",
                "RETURN 2",
            ),
            (
                "CREATE CLIENT FUNCTION examples.bad() RETURNS BOOLEAN IS STATE count TEXT; BEGIN END;",
                "CLIENT state blocks accept only a single RETURN statement",
                "END",
            ),
            (
                "CREATE CLIENT FUNCTION examples.bad() RETURNS BOOLEAN IS BEGIN IF x THEN RETURN TRUE; END;",
                "expected keyword IF",
                "END",
            ),
        ];

        for (source, message, marker) in cases {
            let parsed = parse(source);
            assert!(parsed.client_functions().is_empty(), "source: {source}");
            let diagnostic = parsed
                .diagnostics()
                .iter()
                .find(|diagnostic| diagnostic.message == message)
                .unwrap_or_else(|| panic!("source: {source}: {:?}", parsed.diagnostics()));
            assert_eq!(diagnostic.code, "ORNA0001");
            let start = source.find(marker).expect("diagnostic marker");
            assert_eq!(diagnostic.span.start, start, "source: {source}");
            // The diagnostic names the offending keyword token, which is
            // the first word of the marker.
            let token = marker.split_whitespace().next().expect("marker token");
            assert_eq!(diagnostic.span.end, start + token.len(), "source: {source}");
        }
    }

    #[test]
    fn accepts_multiple_no_state_returns_and_trivia_before_block_terminators() {
        let source = "CREATE CLIENT FUNCTION examples.control() RETURNS INTEGER IS\n\
            BEGIN\n\
                IF TRUE THEN\n\
                    RETURN 1;\n\
                END -- conditional terminator\n\
                IF -- keyword and semicolon trivia\n\
                ;\n\
                RETURN 2;\n\
                RETURN 3;\n\
            END;";
        let parsed = parse(source);

        assert!(
            parsed.diagnostics().is_empty(),
            "{:?}",
            parsed.diagnostics()
        );
        let ClientFunctionBody::StateBlock(block) = &parsed.client_functions()[0].body else {
            panic!("expected a procedural body");
        };
        assert_eq!(block.statements.len(), 2);
        assert!(matches!(
            block.statements[0],
            ClientProceduralStatement::If(_)
        ));
        assert!(matches!(
            block.statements[1],
            ClientProceduralStatement::Return(_)
        ));
        assert!(matches!(
            block.return_expression,
            Some(ClientExpression::IntegerLiteral { value: 3, .. })
        ));
    }

    #[test]
    fn keeps_server_and_client_function_reports_separate() {
        let source = "CREATE SERVER FUNCTION examples.server() RETURNS ROWS (value BOOL) AS SELECT t.value FROM examples.item t;\n\
            CREATE CLIENT FUNCTION examples.client() RETURNS BOOLEAN RETURN TRUE;";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.server_functions().len(), 1);
        assert_eq!(parsed.server_functions()[0].name.parts[1].text, "server");
        assert_eq!(parsed.client_functions().len(), 1);
        assert_eq!(parsed.client_functions()[0].name.parts[1].text, "client");
    }

    #[test]
    fn client_return_type_diagnostics_do_not_change_server_parsing() {
        let source = "CREATE SERVER FUNCTION tasks.bad() RETURNS RETURN TRUE;";
        let parsed = parse(source);

        assert!(parsed.server_functions().is_empty());
        assert!(parsed.client_functions().is_empty());
        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(parsed.diagnostics()[0].code, "ORNA0001");
        assert_eq!(parsed.diagnostics()[0].message, "expected keyword AS");
        let start = source.find("TRUE").expect("offending SERVER body token");
        assert_eq!(
            parsed.diagnostics()[0].span,
            SourceSpan {
                start,
                end: start + 4,
            }
        );
    }

    fn assert_body_diagnostic(
        parameters: &str,
        result_column: &str,
        body: &str,
        message: &str,
        marker: &str,
        span_offset: usize,
        span_length: usize,
    ) {
        let source = format!(
            "CREATE SERVER FUNCTION tasks.bad({parameters}) RETURNS ROWS ({result_column}) AS {body};"
        );
        let parsed = parse(&source);

        assert_eq!(parsed.syntax().text(), source);
        assert!(parsed.server_functions().is_empty(), "invalid body: {body}");
        assert_eq!(parsed.diagnostics().len(), 1, "invalid body: {body}");
        let diagnostic = &parsed.diagnostics()[0];
        assert_eq!(diagnostic.code, "ORNA0001");
        assert_eq!(diagnostic.message, message);
        let offending = source.find(marker).expect("offending syntax exists") + span_offset;
        assert_eq!(diagnostic.span.start, offending);
        assert_eq!(diagnostic.span.end, offending + span_length);
    }

    fn assert_named_type(type_specification: &TypeSpecification, expected: &str) {
        match type_specification {
            TypeSpecification::Named(name) => {
                assert_eq!(
                    name.parts
                        .iter()
                        .map(|part| part.text.as_str())
                        .collect::<Vec<_>>(),
                    expected.split('.').collect::<Vec<_>>()
                );
            }
            _ => panic!("field must use a named type"),
        }
    }

    fn assert_standard_large_object_type(
        type_specification: &TypeSpecification,
        expected_kind: StandardLargeObjectKind,
        expected_source: &str,
    ) {
        match type_specification {
            TypeSpecification::StandardLargeObject { kind, source } => {
                assert_eq!(*kind, expected_kind);
                assert_eq!(source.text, expected_source);
            }
            _ => panic!("field must use a standard large object type"),
        }
    }

    fn assert_reference_type(
        type_specification: &TypeSpecification,
        first: &str,
        second: &str,
        third: &str,
    ) {
        match type_specification {
            TypeSpecification::Reference { target, .. } => {
                let TypeSpecification::Named(target) = target.as_ref() else {
                    panic!("reference target must be a named type");
                };
                assert_eq!(target.parts[0].text, first);
                assert_eq!(target.parts[1].text, second);
                if third.is_empty() {
                    assert_eq!(target.parts.len(), 2);
                } else {
                    assert_eq!(target.parts[2].text, third);
                }
            }
            _ => panic!("type must be a reference"),
        }
    }
    #[test]
    fn cst_snapshot_preserves_schema_tokens_trivia_and_ranges() {
        let source = "CREATE SCHEMA app.core; -- keep";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(
            dump_cst(parsed.syntax().root()),
            "\
node Root [0..31]
  node CreateSchemaStatement [0..23]
    token Word \"CREATE\" [0..6]
    token Whitespace \" \" [6..7]
    token Word \"SCHEMA\" [7..13]
    token Whitespace \" \" [13..14]
    node QualifiedName [14..22]
      token Word \"app\" [14..17]
      token Dot \".\" [17..18]
      token Word \"core\" [18..22]
    token Semicolon \";\" [22..23]
  token Whitespace \" \" [23..24]
  token LineComment \"-- keep\" [24..31]
"
        );
    }

    #[test]
    fn cst_snapshot_records_nested_client_call_structure() {
        let source =
            "CREATE CLIENT FUNCTION app.check(flag BOOL) RETURNS BOOL AS app.is_ready(flag);";
        let parsed = parse(source);

        assert!(parsed.diagnostics().is_empty());
        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(
            dump_cst(parsed.syntax().root()),
            "\
node Root [0..79]
  node CreateClientFunctionStatement [0..79]
    token Word \"CREATE\" [0..6]
    token Whitespace \" \" [6..7]
    token Word \"CLIENT\" [7..13]
    token Whitespace \" \" [13..14]
    token Word \"FUNCTION\" [14..22]
    token Whitespace \" \" [22..23]
    node QualifiedName [23..32]
      token Word \"app\" [23..26]
      token Dot \".\" [26..27]
      token Word \"check\" [27..32]
    token LeftParenthesis \"(\" [32..33]
    node ClientFunctionParameter [33..42]
      token Word \"flag\" [33..37]
      token Whitespace \" \" [37..38]
      node NamedTypeSpecification [38..42]
        node QualifiedName [38..42]
          token Word \"BOOL\" [38..42]
    token RightParenthesis \")\" [42..43]
    token Whitespace \" \" [43..44]
    token Word \"RETURNS\" [44..51]
    token Whitespace \" \" [51..52]
    node ClientFunctionReturnType [52..57]
      node NamedTypeSpecification [52..57]
        node QualifiedName [52..57]
          token Word \"BOOL\" [52..56]
          token Whitespace \" \" [56..57]
    token Word \"AS\" [57..59]
    token Whitespace \" \" [59..60]
    node ClientExpressionBody [60..78]
      node ClientCallExpression [60..78]
        node QualifiedName [60..72]
          token Word \"app\" [60..63]
          token Dot \".\" [63..64]
          token Word \"is_ready\" [64..72]
        token LeftParenthesis \"(\" [72..73]
        node ClientCallArgument [73..77]
          token Word \"flag\" [73..77]
        token RightParenthesis \")\" [77..78]
    token Semicolon \";\" [78..79]
"
        );
    }

    #[test]
    fn cst_snapshot_keeps_recovery_tokens_and_later_declaration() {
        let source = "CREATE SCHEMA app; ? CREATE SCHEMA later;";
        let parsed = parse(source);

        assert_eq!(parsed.diagnostics().len(), 1);
        assert_eq!(parsed.syntax().text(), source);
        assert_eq!(
            dump_cst(parsed.syntax().root()),
            "\
node Root [0..41]
  node CreateSchemaStatement [0..18]
    token Word \"CREATE\" [0..6]
    token Whitespace \" \" [6..7]
    token Word \"SCHEMA\" [7..13]
    token Whitespace \" \" [13..14]
    node QualifiedName [14..17]
      token Word \"app\" [14..17]
    token Semicolon \";\" [17..18]
  token Whitespace \" \" [18..19]
  token Other \"?\" [19..20]
  token Whitespace \" \" [20..21]
  node CreateSchemaStatement [21..41]
    token Word \"CREATE\" [21..27]
    token Whitespace \" \" [27..28]
    token Word \"SCHEMA\" [28..34]
    token Whitespace \" \" [34..35]
    node QualifiedName [35..40]
      token Word \"later\" [35..40]
    token Semicolon \";\" [40..41]
"
        );
    }

    fn dump_cst(root: &rowan::SyntaxNode<crate::parser::OrnaLanguage>) -> String {
        use std::fmt::Write as _;

        fn visit(
            node: &rowan::SyntaxNode<crate::parser::OrnaLanguage>,
            indent: usize,
            output: &mut String,
        ) {
            let range = node.text_range();
            writeln!(
                output,
                "{}node {:?} [{}..{}]",
                " ".repeat(indent),
                node.kind(),
                u32::from(range.start()),
                u32::from(range.end()),
            )
            .expect("writing CST node snapshot");

            for element in node.children_with_tokens() {
                match element {
                    rowan::NodeOrToken::Node(child) => visit(&child, indent + 2, output),
                    rowan::NodeOrToken::Token(token) => {
                        let range = token.text_range();
                        writeln!(
                            output,
                            "{}token {:?} {:?} [{}..{}]",
                            " ".repeat(indent + 2),
                            token.kind(),
                            token.text(),
                            u32::from(range.start()),
                            u32::from(range.end()),
                        )
                        .expect("writing CST token snapshot");
                    }
                }
            }
        }

        let mut output = String::new();
        visit(root, 0, &mut output);
        output
    }
}
