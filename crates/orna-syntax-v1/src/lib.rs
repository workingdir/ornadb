//! Lossless-location syntax front end for the Orna 1.0.0 grammar.
//!
//! This crate deliberately has no dependency on the pre-1.0 `CREATE` AST.
//! Every public span is a half-open UTF-8 byte range, as required by the
//! diagnostic and wire contracts.

mod admission;
mod lexer;
mod parser;

pub use admission::{
    ParseContext, SourceDocumentId, SyntaxAdmissionError, admit_diagnostic, admit_span,
};

pub use lexer::{Keyword, LexError, Token, TokenKind, lex};
pub use parser::{
    Argument, AssignmentOperator, AssignmentTarget, CaseArm, ControlKind, Declaration,
    Diagnostic as SyntaxDiagnostic, DimensionExpr, EntryPoint, EnumPayloadField, EnumVariant, Expr,
    FieldInitializer, FunctionSignature, GenericParameter, ImplMember, Implementation,
    ImportSegment, Item, ItemKind, Label, LambdaParameter, LiteralKind, NameSegment, Parameter,
    Parse, ParseError, Pattern, PatternField, ProtocolMember, RecordField, ReplInput,
    SourcePosition, SourceSpan as SyntaxSpan, Statement, StringSegment, SyntaxTree, TableMember,
    TypeExpr, TypeMember, TypeRepresentation, UnitDefinition, UseTail, Visibility,
    parse_expression, parse_expression_with_file, parse_module, parse_module_with_file, parse_repl,
    parse_repl_with_file, parse_row, parse_row_with_file,
};
