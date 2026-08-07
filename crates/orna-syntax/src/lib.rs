//! Lossless source parsing for the Orna language.
//!
//! This crate recognises schema declarations and object type declarations.
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
            Self::Reference { span, .. } => span,
        }
    }
}

/// A source slice retained for an expression that is not parsed in this slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSlice {
    /// The exact source text in the slice.
    pub text: String,
    /// The byte range of the slice.
    pub span: SourceSpan,
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

/// The body of a server function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerFunctionBody {
    /// An unparsed SQL query retained as exact source for a later SQL stage.
    SqlQuery(SourceSlice),
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

    /// Return successfully parsed server function declarations in source order.
    pub fn server_functions(&self) -> &[ServerFunctionDeclaration] {
        &self.server_functions
    }
}

/// Parse one Orna source unit.
///
/// The parser recognises schema declarations, object type declarations, and
/// server function declarations. It keeps all source bytes in its CST,
/// including bytes in malformed statements.
pub fn parse(source: &str) -> Parse {
    parser::parse(source)
}

#[cfg(test)]
mod tests {
    use super::{
        FunctionReturnType, FunctionSecurity, FunctionTransaction, FunctionVolatility,
        OnDeletePolicy, ServerFunctionBody, TypeSpecification, parse,
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
    fn parses_server_functions_with_rows_returns_and_opaque_sql_bodies() {
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
        AS SELECT tasks.rank(p_before, tasks.weight(1, 2)), p_before FROM tasks.task;";
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
                    query.text,
                    "SELECT tasks.rank(p_before, tasks.weight(1, 2)), p_before FROM tasks.task",
                );
                assert_eq!(
                    query.span.start,
                    source.find("SELECT").expect("query exists")
                );
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
            AS SELECT tasks.audit_event(1, tasks.detail(2, 3));";
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
            TypeSpecification::Named(_) => panic!("project must be a reference"),
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

    fn assert_named_type(type_specification: &TypeSpecification, expected: &str) {
        match type_specification {
            TypeSpecification::Named(name) => {
                assert_eq!(name.parts.len(), 1);
                assert_eq!(name.parts[0].text, expected);
            }
            TypeSpecification::Reference { .. } => panic!("field must use a named type"),
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
            TypeSpecification::Named(_) => panic!("type must be a reference"),
        }
    }
}
