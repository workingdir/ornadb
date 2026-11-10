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

mod blocks;
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
mod units;

use std::collections::BTreeMap;

use orna_syntax::{SyntaxSpan, SyntaxTree};

use crate::{
    Body, BodyId, DefId, EffectSet, Expr, ExprId, IndexVec, Local, LocalId, ModuleId, ModulePath,
    Name, NameUse, Pat, PatId, Program, SourceId, Ty,
};

pub(crate) use classify::{rel_op as relation_operation, table_op as table_operation};
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
/// The engine-embedded `core` and `sys` modules (D12) are lowered first, so
/// every caller's sources resolve against them. The result satisfies
/// [`Program::validate`]: lowering represents a malformed input rather than
/// rejecting it, so validation failures are lowering bugs.
#[must_use]
pub fn lower(sources: &[SourceUnit<'_>]) -> Program {
    let embedded = crate::embedded::parse_all();
    let mut units: Vec<SourceUnit<'_>> = embedded
        .iter()
        .enumerate()
        .map(|(index, module)| SourceUnit {
            source: crate::embedded::source_id(index),
            tree: &module.tree,
            path: module.path.clone(),
        })
        .collect();
    units.extend(sources.iter().cloned());
    let mut builder = Builder::new(&units);
    builder.resolve_imports();
    builder.lower_items();
    let mut program = builder.finish();
    crate::effect_pass::run(&mut program);
    crate::rules::run(&mut program);
    program
}

/// One lexical scope while a body is lowered.
pub(crate) struct ScopeFrame {
    start: usize,
    names: BTreeMap<Name, LocalId>,
    locals: Vec<LocalId>,
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
    /// Whether this body is a concurrent body: the lambda passed directly to
    /// `parallel`, `race`, `timeout` or `sys.start`.
    pub(crate) concurrent: bool,
    /// Set while a direct argument of a concurrent call is lowered, so that a
    /// lambda there becomes a concurrent body. Consumed by the lambda.
    pub(crate) lowering_concurrent_argument: bool,
    /// The local scopes, innermost last.
    pub(crate) scopes: Vec<ScopeFrame>,
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
            concurrent: false,
            lowering_concurrent_argument: false,
            scopes: Vec::new(),
            root: None,
        }
    }

    /// Enters a local scope beginning at `start`.
    pub(crate) fn push_scope(&mut self, start: usize) {
        self.scopes.push(ScopeFrame {
            start,
            names: BTreeMap::new(),
            locals: Vec::new(),
        });
    }

    /// Leaves the innermost local scope, which ends at `end`.
    pub(crate) fn pop_scope(&mut self, end: usize) {
        let Some(frame) = self.scopes.pop() else {
            return;
        };
        for local in frame.locals {
            self.locals[local].scope = SyntaxSpan::new(frame.start, end);
        }
    }

    /// Adds one local slot with `name`.
    pub(crate) fn add_local(&mut self, name: Name, ty: Ty, span: SyntaxSpan) -> LocalId {
        let id = self.locals.push(Local {
            name: name.clone(),
            ty,
            mutable: false,
            span,
            scope: SyntaxSpan::new(0, 0),
        });
        if let Some(scope) = self.scopes.last_mut() {
            scope.names.insert(name, id);
            scope.locals.push(id);
        }
        id
    }

    /// The innermost slot bound to `name`.
    pub(crate) fn local(&self, name: &Name) -> Option<LocalId> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.names.get(name).copied())
    }

    /// Whether assigning `local` is an error: it is a capture of this body and either this body is a
    /// concurrent body, or the capture was blocked when the lambda was created.
    pub(crate) fn is_blocked_capture(&self, local: LocalId) -> bool {
        self.captures
            .iter()
            .any(|capture| capture.inner == local && (self.concurrent || capture.blocked))
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
                weight: 1,
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

/// Appends one value-position name use to the module's index.
pub(crate) fn record_value_use(
    program: &mut Program,
    module: ModuleId,
    span: SyntaxSpan,
    name: Name,
    target: crate::NameTarget,
) {
    record_use(
        program,
        module,
        span,
        name,
        crate::UseContext::Value,
        target,
    );
}

/// Appends one name use inside a body to the module's index.
pub(crate) fn record_body_use(
    program: &mut Program,
    module: ModuleId,
    body: BodyId,
    span: SyntaxSpan,
    name: Name,
    context: crate::UseContext,
    target: crate::NameTarget,
) {
    program.modules[module].name_uses.push(NameUse {
        span,
        name,
        context,
        target,
        body: Some(body),
        written: false,
        callee: false,
        shorthand: false,
    });
}

/// Appends one name use to the module's index.
pub(crate) fn record_use(
    program: &mut Program,
    module: ModuleId,
    span: SyntaxSpan,
    name: Name,
    context: crate::UseContext,
    target: crate::NameTarget,
) {
    program.modules[module].name_uses.push(NameUse {
        span,
        name,
        context,
        target,
        body: None,
        written: false,
        callee: false,
        shorthand: false,
    });
}

/// Appends one local binding declaration to the module's index.
pub(crate) fn record_binding_use(
    program: &mut Program,
    module: ModuleId,
    body: BodyId,
    span: SyntaxSpan,
    name: Name,
    local: LocalId,
    shorthand: bool,
) {
    program.modules[module].name_uses.push(NameUse {
        span,
        name,
        context: crate::UseContext::Binding,
        target: crate::NameTarget::Local(local),
        body: Some(body),
        written: true,
        callee: false,
        shorthand,
    });
}

/// Marks the use at `span` as an assignment target or declaration.
pub(crate) fn mark_written(program: &mut Program, module: ModuleId, span: &SyntaxSpan) {
    if let Some(use_) = program.modules[module]
        .name_uses
        .iter_mut()
        .rev()
        .find(|use_| use_.span.start == span.start && use_.span.end == span.end)
    {
        use_.written = true;
    }
}

/// Marks the last use written inside `span` as the callee of a call.
pub(crate) fn mark_callee(program: &mut Program, module: ModuleId, span: &SyntaxSpan) {
    if let Some(use_) = program.modules[module]
        .name_uses
        .iter_mut()
        .rev()
        .find(|use_| span.start <= use_.span.start && use_.span.end <= span.end)
    {
        use_.callee = true;
    }
}
