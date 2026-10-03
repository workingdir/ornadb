//! Lexical constants retained only for the pre-1.0 parser implementation.
//! Editor artifacts are generated from `orna-syntax-v1::editor` instead.

pub(crate) const LINE_COMMENT_START: &str = "--";
pub(crate) const BLOCK_COMMENT_START: &str = "/*";
pub(crate) const BLOCK_COMMENT_END: &str = "*/";
pub(crate) const STRING_DELIMITER: char = '\'';
pub(crate) const QUOTED_IDENTIFIER_DELIMITER: char = '"';
pub(crate) const OPERATORS: &[&str] = &[
    ":=", "=>", "=", "<>", "!=", "<", ">", "<=", ">=", "+", "-", "*", "/", "%", "||", "->", ":",
    "?",
];
