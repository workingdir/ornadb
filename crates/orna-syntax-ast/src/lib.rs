//! The Orna syntax tree: source spans, the item, expression, type and pattern
//! nodes, and the unit and interface-clause node types the parser produces.
//!
//! This crate holds data only. It has no parser, lexer or diagnostics, so a
//! consumer that reads trees does not link the parser.

mod ast;
mod interface;
mod span;

pub use ast::*;
pub use interface::*;
pub use span::*;
