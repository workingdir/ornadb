//! AST to HIR (design sections 2.2 to 2.7 and stage H1).
//!
//! Lowering is total: it never fails. A construct it cannot lower becomes a
//! diagnostic on its module, and the surrounding expression lowers to
//! [`crate::ExprKind::Error`], so every later pass can walk the program without
//! a partial-state case.
//!
//! # The phases
//!
//! 1. [`declare`] creates one [`crate::Module`] per source and one
//!    [`crate::Def`] id per declaration, and registers the module graph.
//! 2. [`Builder::resolve_imports`] resolves every `use` into the module's scope
//!    (`resolve.rs`).
//! 3. [`items`] lowers signatures and then each body: names to slots and
//!    definitions, call arguments to parameter slots, and every call to one
//!    [`crate::Callee`].
//! 4. [`Builder::finish`] writes the definitions and bodies into the program.
//!
//! Every name occurrence is recorded in the module's [`crate::NameUse`] index as
//! it resolves, so the language server and the H1 resolution comparison read one
//! table.

mod calls;
mod classify;
mod control;
mod declare;
mod exprs;
mod free_names;
mod imports;
mod items;
mod literals;
mod pats;
mod types;

use std::collections::BTreeMap;

use orna_syntax::{SyntaxSpan, SyntaxTree};

use crate::{
    Body, BodyId, DefId, EffectSet, Expr, ExprId, IndexVec, Local, LocalId, ModuleId, ModulePath,
    Name, NameUse, Pat, PatId, Program, SourceId, Ty,
};

pub(crate) use declare::Builder;

/// One parsed source and the module path its spans belong to.
#[derive(Clone, Debug)]
pub struct SourceUnit<'a> {
    /// The id that identifies this source in the program's spans.
    pub source: SourceId,
    /// The parsed tree.
    pub tree: &'a SyntaxTree,
    /// The module's canonical path, for example `std.bits` or `app.orders`.
    ///
    /// The caller derives it from the file's namespace (ORNA-NS-001 to
    /// ORNA-NS-004); lowering never reads the filesystem.
    pub path: ModulePath,
}

/// Lowers every source into one program.
///
/// The result satisfies [`Program::validate`]: lowering represents a malformed
/// input rather than rejecting it, so validation failures are lowering bugs.
#[must_use]
pub fn lower(sources: &[SourceUnit<'_>]) -> Program {
    let mut builder = Builder::new(sources);
    builder.resolve_imports();
    builder.lower_items();
    builder.finish()
}

/// One body while it is being lowered: expressions, patterns, slots and the
/// local scopes currently in force.
pub(crate) struct BodyState {
    /// The body's id, allocated when the body was opened.
    pub(crate) id: BodyId,
    /// The definition whose signature owns the body.
    pub(crate) owner: DefId,
    /// The module whose scopes resolve names in this body.
    pub(crate) module: ModuleId,
    /// Expressions in lowering order.
    pub(crate) exprs: IndexVec<ExprId, Expr>,
    /// Patterns in lowering order.
    pub(crate) pats: IndexVec<PatId, Pat>,
    /// Slots in allocation order.
    pub(crate) locals: IndexVec<LocalId, Local>,
    /// The parameter slots, in parameter order.
    pub(crate) params: Vec<LocalId>,
    /// Captures, for a lambda body.
    pub(crate) captures: Vec<crate::Capture>,
    /// The local scopes, innermost last.
    pub(crate) scopes: Vec<BTreeMap<Name, LocalId>>,
    /// The body's root expression, once it has one.
    pub(crate) root: Option<ExprId>,
}

impl BodyState {
    /// Opens one body.
    pub(crate) fn new(id: BodyId, owner: DefId, module: ModuleId) -> Self {
        Self {
            id,
            owner,
            module,
            exprs: IndexVec::new(),
            pats: IndexVec::new(),
            locals: IndexVec::new(),
            params: Vec::new(),
            captures: Vec::new(),
            scopes: Vec::new(),
            root: None,
        }
    }

    /// Enters a local scope.
    pub(crate) fn push_scope(&mut self) {
        self.scopes.push(BTreeMap::new());
    }

    /// Leaves the innermost local scope.
    pub(crate) fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// Adds one local slot with `name`.
    pub(crate) fn add_local(&mut self, name: Name, ty: Ty, span: SyntaxSpan) -> LocalId {
        let id = self.locals.push(Local {
            name: name.clone(),
            ty,
            mutable: false,
            span,
        });
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, id);
        }
        id
    }

    /// The innermost slot bound to `name`.
    pub(crate) fn local(&self, name: &Name) -> Option<LocalId> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }

    /// Adds one expression and returns its id.
    pub(crate) fn push_expr(&mut self, expr: Expr) -> ExprId {
        self.exprs.push(expr)
    }

    /// Adds one pattern and returns its id.
    pub(crate) fn push_pat(&mut self, pat: Pat) -> PatId {
        self.pats.push(pat)
    }

    /// Finishes the body, using a diagnostic expression as the root when
    /// lowering produced none.
    pub(crate) fn finish(mut self, effects: EffectSet, error_ty: Ty) -> Body {
        let root = self.root.unwrap_or_else(|| {
            self.exprs.push(Expr {
                kind: crate::ExprKind::Error,
                ty: error_ty,
                span: SyntaxSpan::new(0, 0),
            })
        });
        Body {
            owner: self.owner,
            exprs: self.exprs,
            pats: self.pats,
            locals: self.locals,
            params: self.params.into_boxed_slice(),
            captures: self.captures.into_boxed_slice(),
            root,
            effects,
        }
    }
}

/// Appends one name use to the module's index.
pub(crate) fn record_use(
    program: &mut Program,
    module: ModuleId,
    span: SyntaxSpan,
    name: Name,
    target: crate::NameTarget,
) {
    program.modules[module]
        .name_uses
        .push(NameUse { span, name, target });
}
