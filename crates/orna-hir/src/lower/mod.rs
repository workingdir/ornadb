//! AST to HIR (design sections 2.2 to 2.7).
//!
//! Lowering is total: it never fails. A construct it cannot lower becomes an
//! `orna-value` diagnostic on its module, and the surrounding expression
//! lowers to [`crate::ExprKind::Error`], so every later pass can walk the
//! program without a partial-state case.
//!
//! Stage H0 supplies the entry point and its signature only; there are no
//! passes yet, so [`lower`] returns [`crate::Program::empty`]. H1 fills item
//! and body lowering, the resolver and the call classifier behind this exact
//! signature.

use orna_syntax::SyntaxTree;

use crate::{Program, SourceId};

/// One parsed source and the id its spans will refer to.
#[derive(Clone, Copy, Debug)]
pub struct SourceUnit<'a> {
    /// The id that identifies this source in the program's spans.
    pub source: SourceId,
    /// The parsed tree.
    pub tree: &'a SyntaxTree,
}

/// Lowers every source into one program.
///
/// The result satisfies [`Program::validate`]: lowering represents a malformed
/// input rather than rejecting it, so validation failures are lowering bugs.
#[must_use]
pub fn lower(_sources: &[SourceUnit<'_>]) -> Program {
    Program::empty()
}

#[cfg(test)]
mod tests {
    use super::lower;

    #[test]
    fn lowering_no_sources_produces_a_valid_empty_program() {
        let program = lower(&[]);
        assert!(program.is_empty());
        assert_eq!(program.validate(), Ok(()));
    }
}
