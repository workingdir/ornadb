//! Lossless source parsing for the Orna language.
//!
//! This crate recognises supported declarations and SERVER function bodies.
//! All source bytes remain in the CST, including whitespace and comments.

use std::{fmt, ops::Range};

mod lexer;
mod parser;

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

/// A type written in an object field declaration.
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
    /// A typed reference to an object type.
    Reference {
        /// The object type named by the reference.
        target: QualifiedName,
        /// The span from `REF` through the target type name.
        span: SourceSpan,
    },
}

impl TypeSpecification {
    /// Return the span of the written type specification.
    pub fn span(&self) -> &SourceSpan {
        match self {
            Self::Named(name) => &name.span,
            Self::StandardLargeObject { source, .. } => &source.span,
            Self::Reference { span, .. } => span,
        }
    }
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
    /// A bare server function parameter read used as an identity selector.
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
    /// The span from the parameter name through its default expression.
    pub span: SourceSpan,
}

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

/// The declared result shape of a server function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionReturnType {
    /// One scalar or reference value.
    Single(TypeSpecification),
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
}

/// The relational query body of a server function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlQueryBody {
    /// The exact source text for the query body, without the declaration terminator.
    pub source: SourceSlice,
    /// The typed Orna query syntax.
    pub query: SelectQuery,
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
}

impl MutationValue {
    /// Return the complete source span for this value.
    pub fn span(&self) -> &SourceSpan {
        match self {
            Self::Parameter(name) => &name.span,
            Self::BooleanLiteral { source, .. } | Self::NullLiteral { source } => &source.span,
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
    /// The alias written inside the `RETURNING REF(...)` expression.
    pub returning_alias: NamePart,
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
    field_renames: Vec<FieldRenameDeclaration>,
    server_functions: Vec<ServerFunctionDeclaration>,
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

    /// Return successfully parsed field rename declarations in source order.
    pub fn field_renames(&self) -> &[FieldRenameDeclaration] {
        &self.field_renames
    }

    /// Return successfully parsed server function declarations in source order.
    pub fn server_functions(&self) -> &[ServerFunctionDeclaration] {
        &self.server_functions
    }
}

/// Parse one Orna source unit.
///
/// The parser recognises schema declarations, object type declarations, field
/// rename declarations, and server function declarations. It keeps all source
/// bytes in its CST, including bytes in malformed statements.
pub fn parse(source: &str) -> Parse {
    parser::parse(source)
}

#[cfg(test)]
mod tests {
    use super::{
        FunctionReturnType, FunctionSecurity, FunctionTransaction, FunctionVolatility, InsertValue,
        MutationValue, NullOrdering, OnDeletePolicy, OrderingDirection, QueryExpression,
        ServerFunctionBody, SourceSpan, StandardLargeObjectKind, TypeSpecification, parse,
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
            FunctionReturnType::Single(_) => panic!("tasks.overdue must return rows"),
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
            | ServerFunctionBody::SqlDelete(_) => {
                panic!("tasks.overdue must use a SELECT body")
            }
        }
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
            FunctionReturnType::Rows { .. } => panic!("tasks.reopen must return one reference"),
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
            FunctionReturnType::Rows { .. } => panic!("files.encode must return one value"),
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
            FunctionReturnType::Single(_) => panic!("files.describe must return rows"),
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
            TypeSpecification::Named(_) | TypeSpecification::Reference { .. } => {
                panic!("body must use the standard large object AST form")
            }
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
                assert_eq!(target.parts[0].text, "tasks");
                assert_eq!(target.parts[1].text, "project");
            }
            TypeSpecification::Named(_) | TypeSpecification::StandardLargeObject { .. } => {
                panic!("project must be a reference")
            }
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
            "expected a CREATE or ALTER declaration"
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
                assert_eq!(name.parts.len(), 1);
                assert_eq!(name.parts[0].text, expected);
            }
            TypeSpecification::StandardLargeObject { .. } | TypeSpecification::Reference { .. } => {
                panic!("field must use a named type")
            }
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
            TypeSpecification::Named(_) | TypeSpecification::Reference { .. } => {
                panic!("field must use a standard large object type")
            }
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
                assert_eq!(target.parts[0].text, first);
                assert_eq!(target.parts[1].text, second);
                if third.is_empty() {
                    assert_eq!(target.parts.len(), 2);
                } else {
                    assert_eq!(target.parts[2].text, third);
                }
            }
            TypeSpecification::Named(_) | TypeSpecification::StandardLargeObject { .. } => {
                panic!("type must be a reference")
            }
        }
    }
}
