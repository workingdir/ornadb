//! Table names read as relations when they appear in expression position.

use orna_syntax_ast::Expr;

use super::{Context, Scope, dotted_name_path, function_root_name};

impl Context<'_, '_> {
    /// A table name read as a relation: a bare `music` piped into a relation
    /// operation, or a root-qualified `main.music` piped or evaluated alone.
    /// Locals, functions, aliases and namespace-qualified functions keep their
    /// precedence over a table name, mirroring [`Expr::Name`] resolution in
    /// [`Context::evaluate_node`].
    pub(super) fn admitted_table_relation(&self, lhs: &Expr, scope: &Scope) -> Option<String> {
        // Without an effect handler nothing is admitted, so pure evaluation
        // returns before any name work.
        let effects = self.effects.as_deref()?;
        // The root is checked before the dotted path is built, so a field read
        // on a local never allocates.
        let root = function_root_name(lhs)?;
        if scope.0.contains_key(root) {
            return None;
        }
        let path = dotted_name_path(lhs)?;
        if !self.mode.is_repl() && self.functions.contains_key(&path) {
            return None;
        }
        if self
            .alias_target(&path)
            .is_some_and(|name| self.functions.contains_key(name))
        {
            return None;
        }
        if let Some(namespace) = self.namespace.as_deref()
            && self.functions.contains_key(&format!("{namespace}.{path}"))
        {
            return None;
        }
        effects.admitted_relation_source(&path)
    }
}
