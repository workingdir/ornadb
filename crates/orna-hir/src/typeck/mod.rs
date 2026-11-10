//! Expression typing for the annotated subset (stage H2a, design sections 2.3
//! and 6.3 H2).
//!
//! The type pass walks every body of a lowered program once, in a fixpoint over
//! declaration order so a caller can use a result the callee's body infers:
//!
//! 1. Every binding slot whose type the source did not write gets an inference
//!    variable ([`crate::infer::InferCtxt`]).
//! 2. `let` annotations and annotated parameters are the expected types their
//!    initializers are checked against.
//! 3. Calls are checked per [`Callee`]: a source function against its resolved
//!    signature, a constructor against its fields, a first-class value against
//!    its `Fn` type, and a member call (`Callee::Pending`) is settled into the
//!    table operation or relation stage the receiver's type names.
//! 4. A function whose result annotation is absent gets the result its body
//!    determines. A private function whose parameter annotation is absent gets
//!    the type its body or default determines (decision D1); a public one is
//!    rejected with ORNA-S020-ANNOTATION.
//!
//! # The boundary
//!
//! H2a is a strict subset. A construct the subset cannot type is reported as a
//! [`NotYet`] reason and the body is left exactly as lowering produced it; that
//! is not a program error and emits no diagnostic. Only a real constraint
//! conflict inside the subset emits a diagnostic: ORNA-INFER-011 for
//! incompatible constraints and ORNA-INFER-007 for a variable that is still
//! unsolved when the body is finished. The differential harness counts the
//! not-yet bodies, so "typed share of the corpus" is a ratchet that only grows
//! (design section 6.3 H2).
//!
//! The interface modules `core.orna` and `sys.orna` are deliberately absent:
//! typed native and `sys` calls need declarations that the parser cannot yet
//! write (`native fn` arrives with stage H5), so those callees stay "not yet on
//! HIR" in this slice. Nothing here invents a second type language for them.

use std::collections::{BTreeMap, BTreeSet};

use orna_syntax::SyntaxSpan;

use crate::diag::{INFER_AMBIGUOUS, INFER_CONFLICT, PUBLIC_ANNOTATION};
use crate::{
    BinOp, Body, BodyId, DefId, DefKind, ExprId, ExprKind, FnBody, HirDiagnostic, LocalId, LogicOp,
    ModuleId, Pat, PatId, Program, RelOp, Stmt, TableOp, Ty, TyKind, UnOp, Visibility,
};

mod call;
mod expr;
mod protocol;
#[cfg(test)]
mod tests;

/// A body the H2a subset cannot type yet, and why.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NotYetReason {
    /// The enclosing private function omits a parameter annotation whose
    /// default is not typed yet (decision D1).
    UnannotatedParameter(DefId),
    /// The construct belongs to a later stage.
    Unsupported(Box<str>),
}

impl NotYetReason {
    /// A human-readable reason for evidence output.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::UnannotatedParameter(def) => format!(
                "the declaration omits a parameter annotation whose default is not typed yet (definition {}); decision D1",
                def.as_u32()
            ),
            Self::Unsupported(reason) => reason.to_string(),
        }
    }
}

/// What the type pass did with one body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BodyOutcome {
    /// The body.
    pub body: BodyId,
    /// The definition that owns the body.
    pub owner: DefId,
    /// The module that definition belongs to.
    pub module: ModuleId,
    /// Whether every expression and binding has a finished static type.
    pub typed: bool,
    /// Why the subset could not type it, when it could not.
    pub not_yet: Option<NotYetReason>,
    /// The number of constraint conflicts reported for the body.
    pub conflicts: usize,
}

/// The result of one type pass over a program.
#[derive(Clone, Debug, Default)]
pub struct TypeCheckSummary {
    /// One entry per top-level body: a function body, a default, a module-level
    /// binding or a module assertion.
    pub outcomes: Vec<BodyOutcome>,
}

impl TypeCheckSummary {
    /// The number of top-level bodies with a finished static type.
    #[must_use]
    pub fn typed(&self) -> usize {
        self.outcomes.iter().filter(|outcome| outcome.typed).count()
    }

    /// The number of top-level bodies the pass examined.
    #[must_use]
    pub fn total(&self) -> usize {
        self.outcomes.len()
    }

    /// Whether every examined body is typed and none reported a conflict.
    #[must_use]
    pub fn accepts(&self) -> bool {
        self.outcomes.iter().all(|outcome| outcome.typed)
    }

    /// The bodies the subset cannot type yet, with their reasons.
    pub fn not_yet(&self) -> impl Iterator<Item = &BodyOutcome> {
        self.outcomes
            .iter()
            .filter(|outcome| outcome.not_yet.is_some())
    }
}

/// Types one lowered program in place.
///
/// Expression and binding types are replaced by their finished static type,
/// `FieldRef::Name` members are settled to the field they name, `Callee::Pending`
/// calls are settled to the operation they denote, and a function's missing
/// result annotation is filled from its body. The program's diagnostics carry
/// every inference error.
pub fn typeck(program: &mut Program) -> TypeCheckSummary {
    let variant_owner = variant_owners(program);
    let mut checker = TypeChecker {
        program,
        variant_owner,
        outcomes: Vec::new(),
        done: BTreeSet::new(),
    };
    checker.run();
    TypeCheckSummary {
        outcomes: checker.outcomes,
    }
}

/// Maps every enum variant definition to its enum.
fn variant_owners(program: &Program) -> BTreeMap<DefId, DefId> {
    let mut owners = BTreeMap::new();
    for (def, definition) in program.defs.iter_enumerated() {
        if let DefKind::Enum(enumeration) = &definition.kind {
            for variant in &enumeration.variants {
                owners.insert(*variant, def);
            }
        }
    }
    owners
}

/// The annotation slots of one function a body can determine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Slot {
    /// The result type.
    Ret,
    /// The parameter at this index.
    Param(usize),
}

/// One type a body determined for an omitted annotation.
struct Solved {
    def: DefId,
    slot: Slot,
    ty: Ty,
}

/// One attempt at one top-level body.
enum Attempt {
    /// The body has finished static types.
    Typed {
        /// Every body checked from this one, including nested lambdas.
        bodies: Vec<(BodyId, Body)>,
        /// The annotations this body determined that the source omitted.
        solved: Vec<Solved>,
    },
    /// The body is inside the program and has a conflict.
    Failed {
        /// The conflicts the pass reported.
        diagnostics: Vec<HirDiagnostic>,
        /// How many conflicts were reported.
        conflicts: usize,
        /// The reasons, for evidence.
        detail: Box<str>,
    },
    /// The subset cannot type the body yet; nothing was changed.
    NotYet(NotYetReason),
}

struct TypeChecker<'a> {
    program: &'a mut Program,
    variant_owner: BTreeMap<DefId, DefId>,
    outcomes: Vec<BodyOutcome>,
    done: BTreeSet<BodyId>,
}

impl TypeChecker<'_> {
    fn run(&mut self) {
        let mut pending = self.top_level_bodies();
        let mut reasons: BTreeMap<BodyId, NotYetReason> = BTreeMap::new();
        loop {
            let mut progressed = false;
            let mut still_pending = Vec::new();
            for body in pending {
                if self.done.contains(&body) {
                    continue;
                }
                match self.attempt(body) {
                    Attempt::Typed { bodies, solved } => {
                        progressed = true;
                        self.commit_typed(body, bodies, solved);
                    }
                    Attempt::Failed {
                        diagnostics,
                        conflicts,
                        detail,
                    } => {
                        progressed = true;
                        self.commit_failed(body, diagnostics, conflicts, &detail);
                    }
                    Attempt::NotYet(reason) => {
                        reasons.insert(body, reason);
                        still_pending.push(body);
                    }
                }
            }
            pending = still_pending;
            if !progressed || pending.is_empty() {
                break;
            }
        }
        for body in pending {
            let owner = self.program.bodies[body].owner;
            let module = self.program.defs[owner].module;
            let reason = reasons.remove(&body).unwrap_or(NotYetReason::Unsupported(
                "the body could not be typed on HIR".into(),
            ));
            self.outcomes.push(BodyOutcome {
                body,
                owner,
                module,
                typed: false,
                not_yet: Some(reason),
                conflicts: 0,
            });
        }
    }

    /// The bodies the pass walks: every function body with an Orna body, every
    /// parameter default, every module-level binding and every assertion.
    fn top_level_bodies(&self) -> Vec<BodyId> {
        let mut bodies = Vec::new();
        for definition in self.program.defs.iter() {
            match &definition.kind {
                DefKind::Fn(function) => {
                    if let FnBody::Source(body) = function.body {
                        bodies.push(body);
                    }
                    for parameter in &function.params {
                        if let Some(default) = parameter.default {
                            bodies.push(default);
                        }
                    }
                }
                DefKind::Let(binding) => bodies.push(binding.body),
                DefKind::Assertion(assertion) => bodies.push(assertion.body),
                _ => {}
            }
        }
        bodies.sort_by_key(|body| body.as_u32());
        bodies.dedup();
        bodies
    }

    /// Whether a parameter or result slot still has no written type.
    fn is_error(&self, ty: Ty) -> bool {
        matches!(self.program.tys.kind(ty), Some(TyKind::Error))
    }

    /// Attempts one top-level body.
    fn attempt(&mut self, body_id: BodyId) -> Attempt {
        let owner = self.program.bodies[body_id].owner;
        let owner_kind = self.program.defs[owner].kind.clone();
        let public = self.program.defs[owner].vis == Visibility::Public;
        let body_params = self.program.bodies[body_id].params.clone();
        let (expected, root_slot, omitted) = match &owner_kind {
            DefKind::Fn(function) => {
                let is_main = matches!(function.body, FnBody::Source(id) if id == body_id);
                if is_main {
                    let omitted = function
                        .params
                        .iter()
                        .enumerate()
                        .filter(|(_, parameter)| !parameter.written)
                        .map(|(index, _)| index)
                        .collect::<Vec<_>>();
                    if public && !omitted.is_empty() {
                        let span = self.program.defs[owner].span.clone();
                        let diagnostics = omitted
                            .iter()
                            .map(|&index| {
                                HirDiagnostic::new(
                                    PUBLIC_ANNOTATION,
                                    format!(
                                        "public function parameter `{}` needs a type annotation (decision D1)",
                                        function.params[index].name.as_str()
                                    ),
                                    span.clone(),
                                )
                            })
                            .collect::<Vec<_>>();
                        return Attempt::Failed {
                            conflicts: diagnostics.len(),
                            diagnostics,
                            detail: "a public function omits a parameter annotation".into(),
                        };
                    }
                    if omitted
                        .iter()
                        .any(|&index| function.params[index].default.is_some())
                    {
                        return Attempt::NotYet(NotYetReason::UnannotatedParameter(owner));
                    }
                    let infer = self.is_error(function.ret);
                    let expected = if infer { None } else { Some(function.ret) };
                    (expected, infer.then_some(Slot::Ret), omitted)
                } else if let Some(index) = function
                    .params
                    .iter()
                    .position(|parameter| parameter.default == Some(body_id))
                {
                    let is_omitted = !function.params[index].written;
                    if is_omitted && public {
                        return Attempt::NotYet(NotYetReason::UnannotatedParameter(owner));
                    }
                    let expected = if is_omitted {
                        None
                    } else {
                        Some(function.params[index].ty)
                    };
                    (
                        expected,
                        is_omitted.then_some(Slot::Param(index)),
                        Vec::new(),
                    )
                } else {
                    return Attempt::NotYet(NotYetReason::Unsupported(
                        "this body is not a function body or default".into(),
                    ));
                }
            }
            DefKind::Let(_) | DefKind::Assertion(_) => (None, None, Vec::new()),
            _ => {
                return Attempt::NotYet(NotYetReason::Unsupported(
                    "this definition's body is outside the H2a subset".into(),
                ));
            }
        };

        let mut check = BodyCheck::new(&mut *self.program, &self.variant_owner, owner);
        let root = check.check_root(body_id, expected);
        let root_ty = match root {
            Ok(root_ty) => root_ty,
            Err(reason) => return Attempt::NotYet(reason),
        };
        let mut solved = Vec::new();
        if let Some(slot) = root_slot {
            solved.push(Solved {
                def: owner,
                slot,
                ty: check.zonk(root_ty),
            });
        }
        for index in omitted {
            let ty = check.local_ty(body_id, body_params[index]);
            solved.push(Solved {
                def: owner,
                slot: Slot::Param(index),
                ty,
            });
        }
        let (bodies, mut diagnostics, conflicts) = check.finish();
        if conflicts == 0 {
            Attempt::Typed { bodies, solved }
        } else {
            let detail = diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            diagnostics.shrink_to_fit();
            Attempt::Failed {
                diagnostics,
                conflicts,
                detail: detail.into_boxed_str(),
            }
        }
    }

    fn commit_typed(&mut self, body: BodyId, bodies: Vec<(BodyId, Body)>, solved: Vec<Solved>) {
        let owner = self.program.bodies[body].owner;
        let module = self.program.defs[owner].module;
        for (id, checked) in bodies {
            self.program.bodies[id] = checked;
            self.done.insert(id);
        }
        for Solved { def, slot, ty } in solved {
            if let DefKind::Fn(function) = &mut self.program.defs[def].kind {
                match slot {
                    Slot::Ret => function.ret = ty,
                    Slot::Param(index) => function.params[index].ty = ty,
                }
            }
        }
        self.outcomes.push(BodyOutcome {
            body,
            owner,
            module,
            typed: true,
            not_yet: None,
            conflicts: 0,
        });
    }

    fn commit_failed(
        &mut self,
        body: BodyId,
        diagnostics: Vec<HirDiagnostic>,
        conflicts: usize,
        detail: &str,
    ) {
        let owner = self.program.bodies[body].owner;
        let module = self.program.defs[owner].module;
        self.program.modules[module].diagnostics.extend(diagnostics);
        self.done.insert(body);
        self.outcomes.push(BodyOutcome {
            body,
            owner,
            module,
            typed: false,
            not_yet: Some(NotYetReason::Unsupported(
                format!("the body has {conflicts} type conflict(s): {detail}").into_boxed_str(),
            )),
            conflicts,
        });
    }
}

/// One body while it is being typed.
struct BodyCheck<'a> {
    program: &'a mut Program,
    variant_owner: &'a BTreeMap<DefId, DefId>,
    /// The definition whose body is being checked; its generics bound the
    /// `Param` types the body names.
    owner: DefId,
    infer: crate::infer::InferCtxt,
    diagnostics: Vec<HirDiagnostic>,
    conflicts: usize,
    working: BTreeMap<BodyId, Body>,
    /// The expected result type of the function whose body is being checked,
    /// innermost last.
    returns: Vec<Option<Ty>>,
}

impl<'a> BodyCheck<'a> {
    fn new(
        program: &'a mut Program,
        variant_owner: &'a BTreeMap<DefId, DefId>,
        owner: DefId,
    ) -> Self {
        Self {
            program,
            variant_owner,
            owner,
            infer: crate::infer::InferCtxt::new(),
            diagnostics: Vec::new(),
            conflicts: 0,
            working: BTreeMap::new(),
            returns: Vec::new(),
        }
    }

    fn take(&mut self, id: BodyId) -> Body {
        self.working
            .remove(&id)
            .unwrap_or_else(|| self.program.bodies[id].clone())
    }

    fn put(&mut self, id: BodyId, body: Body) {
        self.working.insert(id, body);
    }

    fn zonk(&mut self, ty: Ty) -> Ty {
        let infer = &self.infer;
        infer.zonk(&mut self.program.tys, ty)
    }

    fn kind_of(&self, ty: Ty) -> Option<TyKind> {
        self.program.tys.kind(ty).cloned()
    }

    fn resolved(&self, ty: Ty) -> Ty {
        self.infer.resolve(&self.program.tys, ty)
    }

    /// The current type of one binding slot of a body checked here, zonked.
    fn local_ty(&mut self, body: BodyId, local: LocalId) -> Ty {
        let ty = match self.working.get(&body) {
            Some(checked) => checked.locals[local].ty,
            None => self.program.bodies[body].locals[local].ty,
        };
        self.zonk(ty)
    }

    fn error(&mut self) -> Ty {
        self.program.tys.error()
    }

    fn intern(&mut self, kind: TyKind) -> Ty {
        self.program.tys.intern(kind)
    }

    /// Checks a body's root against its expected result type.
    fn check_root(&mut self, id: BodyId, expected: Option<Ty>) -> Result<Ty, NotYetReason> {
        let mut body = self.take(id);
        self.prepare_locals(&mut body, false);
        let root = body.root;
        let span = body.exprs[root].span.clone();
        self.returns.push(expected);
        let result = self.infer_expr(&mut body, root, expected);
        self.returns.pop();
        let ty = result?;
        if let Some(expected) = expected {
            self.unify(ty, expected, span);
        }
        self.put(id, body);
        Ok(ty)
    }

    /// Gives every binding slot without a written type an inference variable.
    fn prepare_locals(&mut self, body: &mut Body, skip_params: bool) {
        let params: BTreeSet<LocalId> = if skip_params {
            body.params.iter().copied().collect()
        } else {
            BTreeSet::new()
        };
        let pending = body
            .locals
            .iter_enumerated()
            .filter(|(id, local)| {
                !params.contains(id)
                    && matches!(self.program.tys.kind(local.ty), Some(TyKind::Error))
            })
            .map(|(id, local)| (id, local.span.clone()))
            .collect::<Vec<_>>();
        for (id, span) in pending {
            let var = self.infer.new_var(&mut self.program.tys, span);
            body.locals[id].ty = var;
        }
    }

    /// Finishes the body: replaces solved variables, reports unsolved ones and
    /// returns the bodies to commit.
    fn finish(mut self) -> (Vec<(BodyId, Body)>, Vec<HirDiagnostic>, usize) {
        let ids = self.working.keys().copied().collect::<Vec<_>>();
        for id in ids {
            let Some(mut body) = self.working.remove(&id) else {
                continue;
            };
            let exprs = body
                .exprs
                .iter_enumerated()
                .map(|(id, _)| id)
                .collect::<Vec<_>>();
            for expr in exprs {
                let ty = body.exprs[expr].ty;
                body.exprs[expr].ty = self.zonk(ty);
            }
            let locals = body
                .locals
                .iter_enumerated()
                .map(|(id, _)| id)
                .collect::<Vec<_>>();
            for local in locals {
                let ty = body.locals[local].ty;
                body.locals[local].ty = self.zonk(ty);
            }
            self.working.insert(id, body);
        }
        for (var, span) in self.infer.unsolved() {
            self.conflicts += 1;
            self.diagnostics.push(HirDiagnostic::new(
                INFER_AMBIGUOUS,
                format!(
                    "the type of this expression is ambiguous; add an annotation \
                     (inference variable {} remains unsolved)",
                    var.as_u32()
                ),
                span,
            ));
        }
        let bodies = self.working.into_iter().collect::<Vec<_>>();
        (bodies, self.diagnostics, self.conflicts)
    }

    /// Reports one constraint conflict (ORNA-INFER-011).
    fn conflict(&mut self, message: impl Into<Box<str>>, span: SyntaxSpan) {
        self.conflicts += 1;
        self.diagnostics
            .push(HirDiagnostic::new(INFER_CONFLICT, message.into(), span));
    }

    /// Unifies two types, reporting a conflict with both sides printed.
    fn unify(&mut self, left: Ty, right: Ty, span: SyntaxSpan) {
        let left = self.resolved(left);
        let right = self.resolved(right);
        if let Err(conflict) = self.infer.unify(&mut self.program.tys, left, right) {
            let left_text = crate::print::print_type(self.program, conflict.left);
            let right_text = crate::print::print_type(self.program, conflict.right);
            self.conflict(
                format!("the constraints {left_text} and {right_text} cannot both hold"),
                span,
            );
        }
    }

    /// Checks a value against its expected type, applying the lift rule of
    /// decision D3: a `T` where `T?` is expected is accepted as `Some(T)`. The
    /// value keeps its flat representation, so no node is added. Returns the
    /// type the value has at the site: `expected` when the lift applies.
    fn coerce(&mut self, actual: Ty, expected: Ty, span: SyntaxSpan) -> Ty {
        let actual_resolved = self.resolved(actual);
        let expected_resolved = self.resolved(expected);
        if let Some(TyKind::Option(inner)) = self.kind_of(expected_resolved) {
            let lifts = match self.kind_of(actual_resolved) {
                Some(TyKind::Option(_) | TyKind::Never | TyKind::Error) | None => false,
                Some(_) => true,
            };
            if lifts {
                self.unify(actual, inner, span);
                return expected;
            }
        }
        self.unify(actual, expected, span);
        actual
    }

    /// Checks one expression, using `expected` for the constructs where context
    /// decides a variable (a record, list, lambda or block).
    fn infer_expr(
        &mut self,
        body: &mut Body,
        id: ExprId,
        expected: Option<Ty>,
    ) -> Result<Ty, NotYetReason> {
        let expr = body.exprs[id].clone();
        let span = expr.span.clone();
        let ty = match (expr.kind, expected) {
            (ExprKind::Record(fields), Some(expected)) => {
                self.check_record(body, &fields, expected, span.clone())?
            }
            (ExprKind::List(elements), Some(expected))
                if matches!(self.kind_of(self.resolved(expected)), Some(TyKind::List(_))) =>
            {
                if let Some(TyKind::List(element)) = self.kind_of(self.resolved(expected)) {
                    for element_expr in &elements {
                        self.infer_expr(body, *element_expr, Some(element))?;
                    }
                }
                expected
            }
            (ExprKind::Lambda { body: child, .. }, Some(expected))
                if matches!(self.kind_of(self.resolved(expected)), Some(TyKind::Fn(_))) =>
            {
                let function = match self.kind_of(self.resolved(expected)) {
                    Some(TyKind::Fn(function)) => function,
                    _ => unreachable!("guarded by the match arm"),
                };
                self.check_lambda(child, Some(function), body)?
            }
            (ExprKind::Block { stmts, tail }, Some(expected)) => {
                self.check_block(body, &stmts, tail, Some(expected), span.clone())?
            }
            (ExprKind::Construct { def, fields }, expected) => {
                self.check_construct(body, def, &fields, expected, span.clone())?
            }
            (ExprKind::If { cond, then, els }, expected) => {
                self.check_if(body, cond, then, els, expected, span.clone())?
            }
            (ExprKind::Case { scrutinee, arms }, expected) => {
                self.check_case(body, scrutinee, &arms, expected, span.clone())?
            }
            (kind, expected) => {
                let ty = self.infer_shallow(body, id, kind, span.clone())?;
                match expected {
                    Some(expected) => self.coerce(ty, expected, span.clone()),
                    None => ty,
                }
            }
        };
        body.exprs[id].ty = ty;
        Ok(ty)
    }

    fn literal_ty(&mut self, literal: &crate::Lit, span: SyntaxSpan) -> Result<Ty, NotYetReason> {
        let kind = match literal {
            crate::Lit::Int(_) => TyKind::Int,
            crate::Lit::Decimal { .. } => TyKind::Decimal,
            crate::Lit::Float(_) => TyKind::Float,
            crate::Lit::Str(_) => TyKind::Str,
            crate::Lit::Bool(_) => TyKind::Bool,
            crate::Lit::Date(_) => TyKind::Date,
            crate::Lit::Instant { .. } => TyKind::Instant,
            crate::Lit::Period(_) => TyKind::Period,
            crate::Lit::Duration { .. } => TyKind::Duration,
            crate::Lit::None => {
                let payload = self.infer.new_var(&mut self.program.tys, span);
                return Ok(self.intern(TyKind::Option(payload)));
            }
        };
        Ok(self.intern(kind))
    }

    fn displayable(&self, ty: Ty) -> bool {
        matches!(
            self.kind_of(self.resolved(ty)),
            Some(
                TyKind::Bool
                    | TyKind::Int
                    | TyKind::Decimal
                    | TyKind::Float
                    | TyKind::Str
                    | TyKind::Uuid
                    | TyKind::Date
                    | TyKind::Instant
                    | TyKind::Duration
                    | TyKind::Period
                    | TyKind::Digest
            )
        )
    }

    fn unary_ty(
        &mut self,
        body: &mut Body,
        op: UnOp,
        operand: ExprId,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        let operand_ty = self.infer_expr(body, operand, None)?;
        let resolved = self.resolved(operand_ty);
        match crate::ops::unary(&mut self.program.tys, op, resolved) {
            Some(result) => Ok(result),
            None => {
                if matches!(self.kind_of(resolved), Some(TyKind::Error)) {
                    return Ok(self.error());
                }
                self.conflict(
                    format!(
                        "the unary operator is not defined for {}",
                        crate::print::print_type(self.program, resolved)
                    ),
                    span,
                );
                Ok(self.error())
            }
        }
    }

    fn binary_ty(
        &mut self,
        body: &mut Body,
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        let left = self.infer_expr(body, lhs, None)?;
        let right = self.infer_expr(body, rhs, None)?;
        let comparison = matches!(
            op,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
        );
        if comparison {
            let before = self.conflicts;
            self.unify(left, right, span.clone());
            if self.conflicts > before {
                return Ok(self.error());
            }
        }
        let left_resolved = self.resolved(left);
        let right_resolved = self.resolved(right);
        if let Some(result) =
            crate::ops::binary(&mut self.program.tys, op, left_resolved, right_resolved)
        {
            return Ok(result);
        }
        let has_variable = matches!(self.kind_of(left_resolved), Some(TyKind::Infer(_)))
            || matches!(self.kind_of(right_resolved), Some(TyKind::Infer(_)));
        if has_variable {
            self.unify(left, right, span.clone());
            let left_resolved = self.resolved(left);
            let right_resolved = self.resolved(right);
            if let Some(result) =
                crate::ops::binary(&mut self.program.tys, op, left_resolved, right_resolved)
            {
                return Ok(result);
            }
        }
        if matches!(self.kind_of(left_resolved), Some(TyKind::Error))
            || matches!(self.kind_of(right_resolved), Some(TyKind::Error))
        {
            return Ok(self.error());
        }
        self.conflict(
            format!(
                "the operator is not defined for {} and {}",
                crate::print::print_type(self.program, left_resolved),
                crate::print::print_type(self.program, right_resolved)
            ),
            span,
        );
        Ok(self.error())
    }

    fn logic_ty(
        &mut self,
        body: &mut Body,
        op: LogicOp,
        lhs: ExprId,
        rhs: ExprId,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        if op == LogicOp::Coalesce {
            let left = self.infer_expr(body, lhs, None)?;
            let left_resolved = self.resolved(left);
            let Some(TyKind::Option(inner)) = self.kind_of(left_resolved) else {
                if matches!(self.kind_of(left_resolved), Some(TyKind::Error)) {
                    return Ok(self.error());
                }
                self.conflict("the left side of `??` must be an option", span);
                return Ok(self.error());
            };
            let right = self.infer_expr(body, rhs, Some(inner))?;
            self.unify(inner, right, span.clone());
            return Ok(self.resolved(inner));
        }
        let bool_ty = self.intern(TyKind::Bool);
        let left = self.infer_expr(body, lhs, Some(bool_ty))?;
        let right = self.infer_expr(body, rhs, Some(bool_ty))?;
        self.unify(left, bool_ty, span.clone());
        self.unify(right, bool_ty, span.clone());
        Ok(bool_ty)
    }

    fn iter_element(&self, ty: Ty) -> Option<Ty> {
        match self.kind_of(ty) {
            Some(TyKind::List(element))
            | Some(TyKind::Relation(element))
            | Some(TyKind::Range(element))
            | Some(TyKind::Stream(element)) => Some(element),
            _ => None,
        }
    }

    /// Types one block. `expected` is the block's expected value type, if any.
    fn check_block(
        &mut self,
        body: &mut Body,
        statements: &[Stmt],
        tail: Option<ExprId>,
        expected: Option<Ty>,
        span: SyntaxSpan,
    ) -> Result<Ty, NotYetReason> {
        for statement in statements {
            self.check_statement(body, statement)?;
        }
        match tail {
            Some(tail) => self.infer_expr(body, tail, expected),
            None => {
                let unit = self.intern(TyKind::Unit);
                if let Some(expected) = expected {
                    self.unify(unit, expected, span);
                }
                Ok(unit)
            }
        }
    }

    fn check_statement(&mut self, body: &mut Body, statement: &Stmt) -> Result<(), NotYetReason> {
        match statement {
            Stmt::Let { pat, init } => {
                let hint = self.pattern_hint(body, *pat);
                let init_ty = self.infer_expr(body, *init, hint)?;
                let span = body.exprs[*init].span.clone();
                self.bind_pattern(body, *pat, init_ty, span)
            }
            Stmt::Assert(value) => {
                let bool_ty = self.intern(TyKind::Bool);
                let span = body.exprs[*value].span.clone();
                let ty = self.infer_expr(body, *value, Some(bool_ty))?;
                self.unify(ty, bool_ty, span);
                Ok(())
            }
            Stmt::Expr(value) => {
                self.infer_expr(body, *value, None)?;
                Ok(())
            }
        }
    }

    /// The written type of a binding pattern, when it has one.
    fn pattern_hint(&self, body: &Body, pat: PatId) -> Option<Ty> {
        match body.pats.get(pat)? {
            Pat::Bind { local } => {
                let ty = body.locals.get(*local)?.ty;
                (!matches!(self.kind_of(ty), Some(TyKind::Infer(_)))).then_some(ty)
            }
            _ => None,
        }
    }

    fn bind_pattern(
        &mut self,
        body: &mut Body,
        pat: PatId,
        ty: Ty,
        span: SyntaxSpan,
    ) -> Result<(), NotYetReason> {
        let pattern = body.pats.get(pat).cloned().ok_or_else(|| {
            NotYetReason::Unsupported("a pattern reference is out of range".into())
        })?;
        match pattern {
            Pat::Wild => Ok(()),
            Pat::Bind { local } => {
                let existing = body.locals[local].ty;
                let local_span = body.locals[local].span.clone();
                self.unify(existing, ty, local_span);
                Ok(())
            }
            Pat::Lit(literal) => {
                let literal_ty = self.literal_ty(&literal, span.clone())?;
                self.unify(ty, literal_ty, span);
                Ok(())
            }
            Pat::Tuple(elements) => {
                let mut variables = Vec::with_capacity(elements.len());
                for _ in &elements {
                    variables.push(self.infer.new_var(&mut self.program.tys, span.clone()));
                }
                let tuple = self.intern(TyKind::Tuple(variables.clone().into_boxed_slice()));
                self.unify(ty, tuple, span.clone());
                for (element, variable) in elements.iter().zip(variables) {
                    self.bind_pattern(body, *element, variable, span.clone())?;
                }
                Ok(())
            }
            Pat::List {
                prefix,
                rest,
                suffix,
            } => {
                let element = self.infer.new_var(&mut self.program.tys, span.clone());
                let list = self.intern(TyKind::List(element));
                self.unify(ty, list, span.clone());
                for pattern in prefix.iter().chain(suffix.iter()) {
                    self.bind_pattern(body, *pattern, element, span.clone())?;
                }
                if let Some(rest) = rest {
                    let existing = body.locals[rest].ty;
                    self.unify(existing, list, span.clone());
                }
                Ok(())
            }
            Pat::Record(fields) => {
                let resolved = self.resolved(ty);
                match self.kind_of(resolved) {
                    Some(TyKind::Record(declared)) => {
                        for (name, pattern) in &fields {
                            if let Some((_, field_ty)) =
                                declared.iter().find(|(field, _)| field == name)
                            {
                                self.bind_pattern(body, *pattern, *field_ty, span.clone())?;
                            } else {
                                self.conflict(
                                    format!("the record has no field `{}`", name.as_str()),
                                    span.clone(),
                                );
                            }
                        }
                        Ok(())
                    }
                    Some(TyKind::Adt(def, _)) => {
                        let declared = self.nominal_fields(def).ok_or_else(|| {
                            NotYetReason::Unsupported(
                                "a record pattern against this nominal type is not on HIR".into(),
                            )
                        })?;
                        for (name, pattern) in &fields {
                            if let Some((_, field_ty)) =
                                declared.iter().find(|(field, _)| field == name)
                            {
                                self.bind_pattern(body, *pattern, *field_ty, span.clone())?;
                            } else {
                                self.conflict(
                                    format!("the nominal type has no field `{}`", name.as_str()),
                                    span.clone(),
                                );
                            }
                        }
                        Ok(())
                    }
                    Some(TyKind::Error) => Ok(()),
                    _ => Err(NotYetReason::Unsupported(
                        "a record pattern against this value type is not on HIR".into(),
                    )),
                }
            }
            Pat::Ctor { def, fields } => {
                let result = self.ctor_result_ty(def)?;
                self.unify(ty, result, span.clone());
                for (field, pattern) in &fields {
                    let field_ty = self.field_definition_ty(*field)?;
                    self.bind_pattern(body, *pattern, field_ty, span.clone())?;
                }
                Ok(())
            }
        }
    }
}

/// Which collection a relation-stage pipeline runs over.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Container {
    List,
    Relation,
    Stream,
}

/// The table operation a member name denotes.
fn table_operation(name: &str) -> Option<TableOp> {
    crate::lower::table_operation(name)
}

/// The relation stage a member name denotes.
fn relation_operation(name: &str) -> Option<RelOp> {
    crate::lower::relation_operation(name)
}
