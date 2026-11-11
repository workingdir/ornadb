//! The structural validator over a lowered [`Program`].
//!
//! Lowering is total: it never fails, and a malformed program is represented
//! rather than rejected. This module states the invariants every lowered
//! program must satisfy, so a lowering bug surfaces as a failed validation in
//! a test instead of as a wrong answer in production. The invariants are the
//! ones the design states for the ids and tables (section 2.2), bodies and
//! patterns (section 2.4), and calls and the one argument binder (sections 2.6
//! and 2.7). The stage text calls this "the validator of 2.12", but
//! `docs/design/hir.md` has no section 2.12 (its section 2 ends at 2.10), so no
//! invariants are claimed from a section that does not state them. The type and
//! effect passes of H2 and H3 add their own invariants to the same entry point.
//!
//! The invariants checked here are:
//!
//! 1. The `defs` table is partitioned by the modules' `items` lists: a
//!    definition is listed exactly once, by the module it records.
//! 2. Every export names a definition of its own module under that
//!    definition's name; every import target is in range.
//! 3. Every definition's internal references (bodies, fields, keys,
//!    assertions, variants, members, bounds, types) are in range and of the
//!    kind that reference requires.
//! 4. Every body's owner is a definition in range; `params` are the leading
//!    slots `0..n`; captures and the root expression are in range.
//! 5. Every expression, pattern and place reference is in range, every type
//!    is interned, and a call's arguments bind one slot each, in parameter
//!    order, to every parameter of a resolved source or native callee.
//!
//! A `TyKind::Infer` in a finished body is not an error at H0: inference does
//! not exist yet (stage H2), and a program that has not been through it holds
//! no inference variables.

use crate::ids::Idx;
use crate::{
    ArgValue, Body, BodyId, Callee, Def, DefId, DefKind, Expr, ExprId, ExprKind, FieldRef, FnBody,
    LocalId, ModuleId, Name, Pat, PatId, Place, Program, Ty,
};

/// One invariant a lowered program violates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidationError {
    /// A definition is listed by more than one module.
    DefListedTwice {
        /// The definition.
        def: DefId,
    },
    /// No module lists this definition.
    DefUnlisted {
        /// The definition.
        def: DefId,
    },
    /// A definition records a module other than the one that lists it.
    DefModuleMismatch {
        /// The definition.
        def: DefId,
        /// The module that lists it.
        listed_by: ModuleId,
    },
    /// An export names a definition of another module.
    ExportForeignDef {
        /// The exporting module.
        module: ModuleId,
        /// The definition.
        def: DefId,
    },
    /// An export binds a name the definition does not have.
    ExportNameMismatch {
        /// The exporting module.
        module: ModuleId,
        /// The exported name.
        name: Name,
    },
    /// An import names a definition that does not exist.
    ImportDefOutOfRange {
        /// The importing module.
        module: ModuleId,
        /// The definition.
        def: DefId,
    },
    /// An import names a module that does not exist.
    ImportModuleOutOfRange {
        /// The importing module.
        module: ModuleId,
        /// The module it names.
        target: ModuleId,
    },
    /// A definition's body, default or assertion is not in the `bodies` table.
    BodyOutOfRange {
        /// The reference site, for example `fn body`.
        owner: &'static str,
        /// The body.
        body: BodyId,
    },
    /// A body's owner is not in the `defs` table.
    BodyOwnerOutOfRange {
        /// The body.
        body: BodyId,
        /// The owner it names.
        owner: DefId,
    },
    /// A body's owner does not point back at the body.
    BodyOwnerMismatch {
        /// The body.
        body: BodyId,
        /// The owner the body records.
        owner: DefId,
    },
    /// A definition reference is not in the `defs` table.
    DefOutOfRange {
        /// The reference site, for example `table field`.
        owner: &'static str,
        /// The definition.
        def: DefId,
    },
    /// A definition reference names a definition of the wrong kind.
    DefKindMismatch {
        /// The reference site.
        owner: &'static str,
        /// The definition.
        def: DefId,
    },
    /// A type is not in the program's interner.
    TyOutOfRange {
        /// The reference site, for example `expr.ty`.
        owner: &'static str,
        /// The type.
        ty: Ty,
    },
    /// An expression reference is not in its body.
    ExprOutOfRange {
        /// The body.
        body: BodyId,
        /// The expression.
        expr: ExprId,
    },
    /// A pattern reference is not in its body.
    PatOutOfRange {
        /// The body.
        body: BodyId,
        /// The pattern.
        pat: PatId,
    },
    /// A local reference is not in its body.
    LocalOutOfRange {
        /// The body.
        body: BodyId,
        /// The local.
        local: LocalId,
    },
    /// A lambda names a body that does not exist.
    BodyRefOutOfRange {
        /// The referring body.
        body: BodyId,
        /// The referenced body.
        target: BodyId,
    },
    /// A body's root expression is not in its own `exprs` table.
    RootOutOfRange {
        /// The body.
        body: BodyId,
        /// The root.
        root: ExprId,
    },
    /// A body's parameter slot is not a local of the body.
    ParamSlotOutOfRange {
        /// The body.
        body: BodyId,
        /// The slot.
        slot: LocalId,
    },
    /// A body's parameter slot is not the slot its parameter position
    /// requires; parameters occupy slots `0..n` in order.
    ParamSlotNotLeading {
        /// The body.
        body: BodyId,
        /// The parameter position.
        index: usize,
        /// The slot it names.
        slot: LocalId,
    },
    /// Two parameters name the same slot.
    ParamSlotDuplicate {
        /// The body.
        body: BodyId,
        /// The slot.
        slot: LocalId,
    },
    /// A capture names an outer or inner slot that does not exist.
    CaptureOutOfRange {
        /// The body.
        body: BodyId,
        /// The outer slot, which belongs to the enclosing body.
        outer: LocalId,
        /// The inner slot.
        inner: LocalId,
    },
    /// Two captures fill the same inner slot.
    CaptureDuplicate {
        /// The body.
        body: BodyId,
        /// The inner slot.
        inner: LocalId,
    },
    /// A call's arguments are not in parameter order.
    ArgSlotNotIncreasing {
        /// The body.
        body: BodyId,
        /// The call expression.
        call: ExprId,
    },
    /// A call's argument count differs from the callee's parameter count.
    ArgCountMismatch {
        /// The body.
        body: BodyId,
        /// The call expression.
        call: ExprId,
        /// The parameters the callee declares.
        params: usize,
        /// The arguments the call supplies.
        args: usize,
    },
    /// A call's argument slot names no parameter of the callee.
    ArgSlotOutOfRange {
        /// The body.
        body: BodyId,
        /// The call expression.
        call: ExprId,
        /// The slot.
        slot: crate::ParamIdx,
    },
}

mod generics;

impl Program {
    /// Checks every structural invariant and returns all violations.
    ///
    /// An empty program is valid. Lowering is expected to produce a program
    /// with no violations; a violation is a lowering bug.
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();
        self.validate_modules(&mut errors);
        for (def, definition) in self.defs.iter_enumerated() {
            self.validate_def(def, definition, &mut errors);
        }
        for (body_id, body) in self.bodies.iter_enumerated() {
            self.validate_body(body_id, body, &mut errors);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    fn validate_modules(&self, errors: &mut Vec<ValidationError>) {
        let mut listed = vec![false; self.defs.len()];
        for (module_id, module) in self.modules.iter_enumerated() {
            for def in &module.items {
                let Some(definition) = self.defs.get(*def) else {
                    errors.push(ValidationError::DefOutOfRange {
                        owner: "module items",
                        def: *def,
                    });
                    continue;
                };
                if std::mem::replace(&mut listed[def.index()], true) {
                    errors.push(ValidationError::DefListedTwice { def: *def });
                }
                if definition.module != module_id {
                    errors.push(ValidationError::DefModuleMismatch {
                        def: *def,
                        listed_by: module_id,
                    });
                }
            }
            for (name, def) in &module.exports {
                let Some(definition) = self.defs.get(*def) else {
                    errors.push(ValidationError::DefOutOfRange {
                        owner: "module export",
                        def: *def,
                    });
                    continue;
                };
                if definition.module != module_id {
                    errors.push(ValidationError::ExportForeignDef {
                        module: module_id,
                        def: *def,
                    });
                }
                if definition.name != *name {
                    errors.push(ValidationError::ExportNameMismatch {
                        module: module_id,
                        name: name.clone(),
                    });
                }
            }
            for import in &module.imports {
                match import.target {
                    crate::ImportTarget::Def(def) => {
                        if !self.defs.contains(def) {
                            errors.push(ValidationError::ImportDefOutOfRange {
                                module: module_id,
                                def,
                            });
                        }
                    }
                    crate::ImportTarget::Module(target) => {
                        if !self.modules.contains(target) {
                            errors.push(ValidationError::ImportModuleOutOfRange {
                                module: module_id,
                                target,
                            });
                        }
                    }
                }
            }
        }
        for (def, present) in listed.into_iter().enumerate() {
            if !present {
                errors.push(ValidationError::DefUnlisted {
                    def: DefId::from_index(def),
                });
            }
        }
    }

    fn validate_def(&self, from: DefId, definition: &Def, errors: &mut Vec<ValidationError>) {
        match &definition.kind {
            DefKind::Fn(function) => {
                self.validate_ty(function.ret, "fn.ret", errors);
                self.validate_generics(&function.generics, errors);
                for parameter in &function.params {
                    self.validate_ty(parameter.ty, "param.ty", errors);
                    if let Some(body) = parameter.default {
                        self.validate_owned_body(body, from, "param default", errors);
                    }
                }
                match function.body {
                    FnBody::Source(body) => {
                        self.validate_owned_body(body, from, "fn body", errors);
                    }
                    FnBody::Native(_) | FnBody::Signature => {}
                }
            }
            DefKind::Table(table) => {
                self.validate_kinds(&table.fields, "table field", DefKind::is_field, errors);
                self.validate_kinds(&table.key, "table key", DefKind::is_field, errors);
                self.validate_kinds(
                    &table.assertions,
                    "table assertion",
                    DefKind::is_assertion,
                    errors,
                );
            }
            DefKind::Type(ty) => {
                self.validate_generics(&ty.params, errors);
                self.validate_def_refs(&ty.members, "type member", errors);
            }
            DefKind::Enum(enumeration) => {
                self.validate_kinds(
                    &enumeration.variants,
                    "enum variant",
                    |kind| kind.is_variant() || kind.is_unit(),
                    errors,
                );
                self.validate_def_refs(&enumeration.members, "enum implementation", errors);
            }
            DefKind::Variant(variant) => {
                self.validate_kinds(&variant.fields, "variant field", DefKind::is_field, errors);
            }
            DefKind::Protocol(protocol) => {
                self.validate_generics(&protocol.params, errors);
                self.validate_def_refs(&protocol.members, "protocol member", errors)
            }
            DefKind::Impl(implementation) => {
                self.validate_def_refs(
                    std::slice::from_ref(&implementation.owner),
                    "impl owner",
                    errors,
                );
                if let Some(protocol) = implementation.protocol {
                    self.validate_kinds(
                        std::slice::from_ref(&protocol),
                        "impl protocol",
                        DefKind::is_protocol,
                        errors,
                    );
                }
                implementation
                    .protocol_args
                    .iter()
                    .for_each(|ty| self.validate_ty(*ty, "impl argument", errors));
                self.validate_def_refs(&implementation.members, "impl member", errors);
            }
            DefKind::Dimension(dimension) => {
                if let Some(unit) = dimension.unit {
                    self.validate_kinds(
                        std::slice::from_ref(&unit),
                        "dimension unit",
                        DefKind::is_unit,
                        errors,
                    );
                }
            }
            DefKind::Unit(_) => {}
            DefKind::Let(binding) => {
                self.validate_owned_body(binding.body, from, "let body", errors);
            }
            DefKind::Assertion(assertion) => {
                self.validate_owned_body(assertion.body, from, "assertion body", errors);
            }
            DefKind::Field(field) => {
                self.validate_ty(field.ty, "field.ty", errors);
                if let Some(initializer) = field.initializer {
                    self.validate_owned_body(initializer.body(), from, "field initializer", errors);
                }
            }
        }
    }

    fn validate_ty(&self, ty: Ty, owner: &'static str, errors: &mut Vec<ValidationError>) {
        if !self.tys.contains(ty) {
            errors.push(ValidationError::TyOutOfRange { owner, ty });
        }
    }

    fn validate_def_refs(
        &self,
        defs: &[DefId],
        owner: &'static str,
        errors: &mut Vec<ValidationError>,
    ) {
        for def in defs {
            if !self.defs.contains(*def) {
                errors.push(ValidationError::DefOutOfRange { owner, def: *def });
            }
        }
    }

    fn validate_kinds(
        &self,
        defs: &[DefId],
        owner: &'static str,
        expected: fn(&DefKind) -> bool,
        errors: &mut Vec<ValidationError>,
    ) {
        for def in defs {
            match self.defs.get(*def) {
                None => errors.push(ValidationError::DefOutOfRange { owner, def: *def }),
                Some(definition) if !expected(&definition.kind) => {
                    errors.push(ValidationError::DefKindMismatch { owner, def: *def });
                }
                Some(_) => {}
            }
        }
    }

    fn validate_owned_body(
        &self,
        body: BodyId,
        owner: DefId,
        site: &'static str,
        errors: &mut Vec<ValidationError>,
    ) {
        let Some(found) = self.bodies.get(body) else {
            errors.push(ValidationError::BodyOutOfRange { owner: site, body });
            return;
        };
        if found.owner != owner {
            errors.push(ValidationError::BodyOwnerMismatch {
                body,
                owner: found.owner,
            });
        }
    }

    fn validate_body(&self, body_id: BodyId, body: &Body, errors: &mut Vec<ValidationError>) {
        if !self.defs.contains(body.owner) {
            errors.push(ValidationError::BodyOwnerOutOfRange {
                body: body_id,
                owner: body.owner,
            });
        }
        let mut slots = Vec::with_capacity(body.params.len());
        for (index, slot) in body.params.iter().enumerate() {
            if !body.locals.contains(*slot) {
                errors.push(ValidationError::ParamSlotOutOfRange {
                    body: body_id,
                    slot: *slot,
                });
                continue;
            }
            if slot.index() != index {
                errors.push(ValidationError::ParamSlotNotLeading {
                    body: body_id,
                    index,
                    slot: *slot,
                });
            }
            if slots.contains(slot) {
                errors.push(ValidationError::ParamSlotDuplicate {
                    body: body_id,
                    slot: *slot,
                });
            }
            slots.push(*slot);
        }
        let mut captured = Vec::with_capacity(body.captures.len());
        for capture in &body.captures {
            if !body.locals.contains(capture.inner) {
                errors.push(ValidationError::CaptureOutOfRange {
                    body: body_id,
                    outer: capture.outer,
                    inner: capture.inner,
                });
                continue;
            }
            if captured.contains(&capture.inner) {
                errors.push(ValidationError::CaptureDuplicate {
                    body: body_id,
                    inner: capture.inner,
                });
            }
            captured.push(capture.inner);
        }
        if !body.exprs.contains(body.root) {
            errors.push(ValidationError::RootOutOfRange {
                body: body_id,
                root: body.root,
            });
        }
        for (expr_id, expr) in body.exprs.iter_enumerated() {
            self.validate_expr(body_id, body, expr_id, expr, errors);
        }
        for pat in body.pats.iter() {
            self.validate_pat(body_id, body, pat, errors);
        }
    }

    fn validate_expr(
        &self,
        body_id: BodyId,
        body: &Body,
        expr_id: ExprId,
        expr: &Expr,
        errors: &mut Vec<ValidationError>,
    ) {
        self.validate_ty(expr.ty, "expr.ty", errors);
        // The routing match is exhaustive on purpose: a new `ExprKind`
        // variant must decide which group validates it.
        match &expr.kind {
            ExprKind::Lit(_)
            | ExprKind::Error
            | ExprKind::BareName(_)
            | ExprKind::Local(_)
            | ExprKind::Def(_)
            | ExprKind::Unary { .. }
            | ExprKind::Binary { .. }
            | ExprKind::Logic { .. }
            | ExprKind::Range { .. }
            | ExprKind::Call(_)
            | ExprKind::Field { .. }
            | ExprKind::Index { .. }
            | ExprKind::Tuple(_)
            | ExprKind::List(_)
            | ExprKind::Record(_)
            | ExprKind::Construct { .. }
            | ExprKind::Lambda { .. }
            | ExprKind::Interp(_)
            | ExprKind::UnitConvert { .. } => {
                self.validate_value_expr(body_id, body, expr_id, &expr.kind, errors);
            }
            ExprKind::Recover { .. }
            | ExprKind::Block { .. }
            | ExprKind::If { .. }
            | ExprKind::Case { .. }
            | ExprKind::For { .. }
            | ExprKind::While { .. }
            | ExprKind::Loop { .. }
            | ExprKind::Return(_)
            | ExprKind::Break(_)
            | ExprKind::Continue
            | ExprKind::Assign { .. } => {
                self.validate_control_expr(body_id, body, &expr.kind, errors);
            }
        }
    }

    /// Validates the expressions that hold a value: literals, references,
    /// operators and constructors.
    fn validate_value_expr(
        &self,
        body_id: BodyId,
        body: &Body,
        expr_id: ExprId,
        kind: &ExprKind,
        errors: &mut Vec<ValidationError>,
    ) {
        match kind {
            ExprKind::Lit(_) | ExprKind::Error | ExprKind::BareName(_) => {}
            ExprKind::Local(local) => self.validate_local(body_id, body, *local, errors),
            ExprKind::Def(def) => self.validate_def_ref(*def, errors),
            ExprKind::Unary { operand, .. } => {
                self.validate_expr_ref(body_id, body, *operand, errors);
            }
            ExprKind::Binary { lhs, rhs, .. } | ExprKind::Logic { lhs, rhs, .. } => {
                self.validate_expr_ref(body_id, body, *lhs, errors);
                self.validate_expr_ref(body_id, body, *rhs, errors);
            }
            ExprKind::Range { lo, hi, .. } => {
                for operand in [lo, hi].into_iter().flatten() {
                    self.validate_expr_ref(body_id, body, *operand, errors);
                }
            }
            ExprKind::Call(call) => self.validate_call(body_id, body, expr_id, call, errors),
            ExprKind::Field { base, field } => {
                self.validate_expr_ref(body_id, body, *base, errors);
                if let FieldRef::Def(def) = field {
                    self.validate_def_ref(*def, errors);
                }
            }
            ExprKind::Index { base, index } => {
                self.validate_expr_ref(body_id, body, *base, errors);
                self.validate_expr_ref(body_id, body, *index, errors);
            }
            ExprKind::UnitConvert { operand, .. } => {
                self.validate_expr_ref(body_id, body, *operand, errors);
            }
            ExprKind::Tuple(items) | ExprKind::List(items) => {
                for item in items.iter() {
                    self.validate_expr_ref(body_id, body, *item, errors);
                }
            }
            ExprKind::Record(fields) => {
                for (_, value) in fields.iter() {
                    self.validate_expr_ref(body_id, body, *value, errors);
                }
            }
            ExprKind::Construct { def, fields } => {
                self.validate_def_ref(*def, errors);
                for (field, value) in fields.iter() {
                    self.validate_def_ref(*field, errors);
                    self.validate_expr_ref(body_id, body, *value, errors);
                }
            }
            ExprKind::Lambda {
                body: lambda,
                captured,
            } => {
                if !self.bodies.contains(*lambda) {
                    errors.push(ValidationError::BodyRefOutOfRange {
                        body: body_id,
                        target: *lambda,
                    });
                }
                for value in captured.iter() {
                    self.validate_expr_ref(body_id, body, *value, errors);
                }
            }
            ExprKind::Interp(parts) => {
                for part in parts.iter() {
                    if let crate::InterpPart::Expr(value) = part {
                        self.validate_expr_ref(body_id, body, *value, errors);
                    }
                }
            }
            _ => {}
        }
    }

    /// Validates the expressions that control what runs next: recovery,
    /// blocks, branches, loops, abrupt completion and assignment.
    fn validate_control_expr(
        &self,
        body_id: BodyId,
        body: &Body,
        kind: &ExprKind,
        errors: &mut Vec<ValidationError>,
    ) {
        match kind {
            ExprKind::Recover {
                body: recoverable,
                handler,
                arm,
            } => {
                self.validate_expr_ref(body_id, body, *recoverable, errors);
                self.validate_local(body_id, body, *handler, errors);
                self.validate_expr_ref(body_id, body, *arm, errors);
            }
            ExprKind::Block { stmts, tail } => {
                for statement in stmts.iter() {
                    match statement {
                        crate::Stmt::Let { pat, init } => {
                            self.validate_pat_ref(body_id, body, *pat, errors);
                            self.validate_expr_ref(body_id, body, *init, errors);
                        }
                        crate::Stmt::Assert(value) | crate::Stmt::Expr(value) => {
                            self.validate_expr_ref(body_id, body, *value, errors);
                        }
                    }
                }
                if let Some(tail) = tail {
                    self.validate_expr_ref(body_id, body, *tail, errors);
                }
            }
            ExprKind::If { cond, then, els } => {
                self.validate_expr_ref(body_id, body, *cond, errors);
                self.validate_expr_ref(body_id, body, *then, errors);
                if let Some(els) = els {
                    self.validate_expr_ref(body_id, body, *els, errors);
                }
            }
            ExprKind::Case { scrutinee, arms } => {
                self.validate_expr_ref(body_id, body, *scrutinee, errors);
                for arm in arms.iter() {
                    self.validate_pat_ref(body_id, body, arm.pat, errors);
                    if let Some(guard) = arm.guard {
                        self.validate_expr_ref(body_id, body, guard, errors);
                    }
                    self.validate_expr_ref(body_id, body, arm.body, errors);
                }
            }
            ExprKind::For {
                pat,
                iter,
                body: loop_body,
            } => {
                self.validate_pat_ref(body_id, body, *pat, errors);
                self.validate_expr_ref(body_id, body, *iter, errors);
                self.validate_expr_ref(body_id, body, *loop_body, errors);
            }
            ExprKind::While {
                cond,
                body: loop_body,
            } => {
                self.validate_expr_ref(body_id, body, *cond, errors);
                self.validate_expr_ref(body_id, body, *loop_body, errors);
            }
            ExprKind::Loop { body: loop_body } => {
                self.validate_expr_ref(body_id, body, *loop_body, errors);
            }
            ExprKind::Return(value) | ExprKind::Break(value) => {
                if let Some(value) = value {
                    self.validate_expr_ref(body_id, body, *value, errors);
                }
            }
            ExprKind::Continue => {}
            ExprKind::Assign { place, value, .. } => {
                self.validate_place(body_id, body, place, errors);
                self.validate_expr_ref(body_id, body, *value, errors);
            }
            _ => {}
        }
    }

    fn validate_place(
        &self,
        body_id: BodyId,
        body: &Body,
        place: &Place,
        errors: &mut Vec<ValidationError>,
    ) {
        match place {
            Place::Local(local) => self.validate_local(body_id, body, *local, errors),
            Place::Field { base, field } => {
                self.validate_place(body_id, body, base, errors);
                if let FieldRef::Def(def) = field {
                    self.validate_def_ref(*def, errors);
                }
            }
            Place::Index { base, index } => {
                self.validate_place(body_id, body, base, errors);
                self.validate_expr_ref(body_id, body, *index, errors);
            }
        }
    }

    fn validate_pat(
        &self,
        body_id: BodyId,
        body: &Body,
        pat: &Pat,
        errors: &mut Vec<ValidationError>,
    ) {
        match pat {
            Pat::Wild | Pat::Lit(_) => {}
            Pat::Bind { local } => self.validate_local(body_id, body, *local, errors),
            Pat::Tuple(pats) => {
                for pat in pats.iter() {
                    self.validate_pat_ref(body_id, body, *pat, errors);
                }
            }
            Pat::List {
                prefix,
                rest,
                suffix,
            } => {
                for pat in prefix.iter().chain(suffix.iter()) {
                    self.validate_pat_ref(body_id, body, *pat, errors);
                }
                if let Some(rest) = rest {
                    self.validate_local(body_id, body, *rest, errors);
                }
            }
            Pat::Record(fields) => {
                for (_, pat) in fields.iter() {
                    self.validate_pat_ref(body_id, body, *pat, errors);
                }
            }
            Pat::Ctor { def, fields } => {
                self.validate_def_ref(*def, errors);
                for (field, pat) in fields.iter() {
                    self.validate_def_ref(*field, errors);
                    self.validate_pat_ref(body_id, body, *pat, errors);
                }
            }
        }
    }

    fn validate_call(
        &self,
        body_id: BodyId,
        body: &Body,
        call_id: ExprId,
        call: &crate::Call,
        errors: &mut Vec<ValidationError>,
    ) {
        match &call.callee {
            Callee::Source { def, targs }
            | Callee::Native { def, targs }
            | Callee::Sys { def, targs } => {
                self.validate_def_ref(*def, errors);
                for ty in targs.iter() {
                    self.validate_ty(*ty, "call.targ", errors);
                }
                if let Some(Def {
                    kind: DefKind::Fn(function),
                    ..
                }) = self.defs.get(*def)
                {
                    if function.params.len() != call.args.len() {
                        errors.push(ValidationError::ArgCountMismatch {
                            body: body_id,
                            call: call_id,
                            params: function.params.len(),
                            args: call.args.len(),
                        });
                    }
                    for arg in call.args.iter() {
                        if arg.slot.as_u32() as usize >= function.params.len() {
                            errors.push(ValidationError::ArgSlotOutOfRange {
                                body: body_id,
                                call: call_id,
                                slot: arg.slot,
                            });
                        }
                    }
                } else if self.defs.contains(*def) {
                    errors.push(ValidationError::DefKindMismatch {
                        owner: "call callee",
                        def: *def,
                    });
                }
            }
            Callee::TableOp { table, .. } | Callee::Ctor { def: table } => {
                self.validate_def_ref(*table, errors);
            }
            Callee::Relational { .. } | Callee::Intrinsic(_) => {}
            Callee::Pending { base, .. } => self.validate_expr_ref(body_id, body, *base, errors),
            Callee::Value { callee } => self.validate_expr_ref(body_id, body, *callee, errors),
        }
        let mut previous = None;
        for arg in call.args.iter() {
            if let ArgValue::Expr(value) = arg.value {
                self.validate_expr_ref(body_id, body, value, errors);
            }
            if previous.is_some_and(|slot: crate::ParamIdx| arg.slot <= slot) {
                errors.push(ValidationError::ArgSlotNotIncreasing {
                    body: body_id,
                    call: call_id,
                });
            }
            previous = Some(arg.slot);
        }
    }

    fn validate_expr_ref(
        &self,
        body_id: BodyId,
        body: &Body,
        expr: ExprId,
        errors: &mut Vec<ValidationError>,
    ) {
        if !body.exprs.contains(expr) {
            errors.push(ValidationError::ExprOutOfRange {
                body: body_id,
                expr,
            });
        }
    }

    fn validate_pat_ref(
        &self,
        body_id: BodyId,
        body: &Body,
        pat: PatId,
        errors: &mut Vec<ValidationError>,
    ) {
        if !body.pats.contains(pat) {
            errors.push(ValidationError::PatOutOfRange { body: body_id, pat });
        }
    }

    fn validate_local(
        &self,
        body_id: BodyId,
        body: &Body,
        local: LocalId,
        errors: &mut Vec<ValidationError>,
    ) {
        if !body.locals.contains(local) {
            errors.push(ValidationError::LocalOutOfRange {
                body: body_id,
                local,
            });
        }
    }

    fn validate_def_ref(&self, def: DefId, errors: &mut Vec<ValidationError>) {
        if !self.defs.contains(def) {
            errors.push(ValidationError::DefOutOfRange {
                owner: "body reference",
                def,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use num_bigint::BigInt;
    use orna_syntax::SyntaxSpan as SourceSpan;

    use super::ValidationError;
    use crate::{
        Arg, ArgValue, Body, Call, Callee, Def, DefKind, EffectSet, Expr, ExprId, ExprKind, FnBody,
        FnDef, IndexVec, Lit, Local, LocalId, Module, ModuleId, ModuleOrigin, ModulePath, Name,
        ParamDef, ParamIdx, PatId, Program, SourceId, TyKind, Visibility,
    };

    fn span(start: usize) -> SourceSpan {
        SourceSpan::new(start, start + 1)
    }

    /// One program that satisfies every invariant: a module `main` exporting
    /// `run(): Int`, whose body is the literal `1`.
    fn sample_program() -> Program {
        let mut program = Program::empty();
        let int = program.tys.intern(TyKind::Int);
        let mut exprs = IndexVec::new();
        let root = exprs.push(Expr {
            kind: ExprKind::Lit(Lit::Int(BigInt::from(1))),
            ty: int,
            span: span(0),
            weight: 1,
        });
        let body = Body {
            owner: crate::DefId::new(0),
            exprs,
            pats: IndexVec::new(),
            locals: IndexVec::new(),
            params: Box::new([]),
            captures: Box::new([]),
            root,
            effects: EffectSet::empty(),
        };
        let body_id = program.bodies.push(body);
        let module = program.modules.push(Module {
            path: ModulePath::new([Name::new("main")]),
            origin: ModuleOrigin::Project,
            source: SourceId::new(0),
            items: Vec::new(),
            exports: BTreeMap::new(),
            imports: Vec::new(),
            diagnostics: Vec::new(),
            name_uses: Vec::new(),
        });
        let def = program.defs.push(Def {
            module,
            name: Name::new("run"),
            vis: Visibility::Public,
            span: span(0),
            kind: DefKind::Fn(FnDef {
                generics: Vec::new(),
                params: Vec::new(),
                ret: int,
                effects: EffectSet::empty(),
                body: FnBody::Source(body_id),
            }),
        });
        program.modules[module].items.push(def);
        program.modules[module]
            .exports
            .insert(Name::new("run"), def);
        program
    }

    #[test]
    fn a_well_formed_program_validates() {
        assert_eq!(sample_program().validate(), Ok(()));
    }

    #[test]
    fn a_definition_no_module_lists_is_rejected() {
        let mut program = sample_program();
        program.modules[ModuleId::new(0)].items.clear();
        assert_eq!(
            program.validate(),
            Err(vec![ValidationError::DefUnlisted {
                def: crate::DefId::new(0),
            }])
        );
    }

    #[test]
    fn an_export_of_another_modules_definition_is_rejected() {
        let mut program = sample_program();
        let other = program.modules.push(Module {
            path: ModulePath::new([Name::new("other")]),
            origin: ModuleOrigin::Project,
            source: SourceId::new(1),
            items: Vec::new(),
            exports: BTreeMap::new(),
            imports: Vec::new(),
            diagnostics: Vec::new(),
            name_uses: Vec::new(),
        });
        program.modules[other]
            .exports
            .insert(Name::new("run"), crate::DefId::new(0));
        assert_eq!(
            program.validate(),
            Err(vec![ValidationError::ExportForeignDef {
                module: other,
                def: crate::DefId::new(0),
            }])
        );
    }

    #[test]
    fn a_body_whose_root_is_not_in_it_is_rejected() {
        let mut program = sample_program();
        let body = &mut program.bodies[crate::BodyId::new(0)];
        body.root = ExprId::new(7);
        assert_eq!(
            program.validate(),
            Err(vec![ValidationError::RootOutOfRange {
                body: crate::BodyId::new(0),
                root: ExprId::new(7),
            }])
        );
    }

    #[test]
    fn an_expression_reference_outside_its_body_is_rejected() {
        let mut program = sample_program();
        let body_id = crate::BodyId::new(0);
        let int = program.tys.intern(TyKind::Int);
        let value = program.bodies[body_id].exprs.push(Expr {
            kind: ExprKind::Return(Some(ExprId::new(9))),
            ty: int,
            span: span(2),
            weight: 1,
        });
        program.bodies[body_id].root = value;
        assert_eq!(
            program.validate(),
            Err(vec![ValidationError::ExprOutOfRange {
                body: body_id,
                expr: ExprId::new(9),
            }])
        );
    }

    #[test]
    fn a_parameter_that_is_not_the_leading_slot_is_rejected() {
        let mut program = sample_program();
        let int = program.tys.intern(TyKind::Int);
        let body_id = crate::BodyId::new(0);
        let body = &mut program.bodies[body_id];
        body.locals.push(Local {
            name: Name::new("unused"),
            ty: int,
            mutable: false,
            span: span(0),
            scope: span(0),
        });
        body.locals.push(Local {
            name: Name::new("value"),
            ty: int,
            mutable: false,
            span: span(1),
            scope: span(1),
        });
        body.params = Box::new([LocalId::new(1)]);
        let def = program.defs.get_mut(crate::DefId::new(0));
        let Some(Def {
            kind: DefKind::Fn(function),
            ..
        }) = def
        else {
            panic!("the sample program declares one function");
        };
        function.params.push(ParamDef {
            name: Name::new("value"),
            ty: int,
            written: true,
            default: None,
            binder: None,
        });
        assert_eq!(
            program.validate(),
            Err(vec![ValidationError::ParamSlotNotLeading {
                body: body_id,
                index: 0,
                slot: LocalId::new(1),
            }])
        );
    }

    #[test]
    fn a_call_that_does_not_bind_every_parameter_is_rejected() {
        let mut program = sample_program();
        let int = program.tys.intern(TyKind::Int);
        let body_id = crate::BodyId::new(0);
        let argument = program.bodies[body_id].exprs.push(Expr {
            kind: ExprKind::Lit(Lit::Int(BigInt::from(1))),
            ty: int,
            span: span(2),
            weight: 1,
        });
        let root = program.bodies[body_id].exprs.push(Expr {
            kind: ExprKind::Call(Box::new(Call {
                callee: Callee::Source {
                    def: crate::DefId::new(0),
                    targs: Box::new([]),
                },
                callee_span: span(1),
                args: Box::new([Arg {
                    slot: ParamIdx::new(0),
                    value: ArgValue::Expr(argument),
                }]),
                piped: false,
            })),
            ty: int,
            span: span(3),
            weight: 1,
        });
        program.bodies[body_id].root = root;
        assert_eq!(
            program.validate(),
            Err(vec![
                ValidationError::ArgCountMismatch {
                    body: body_id,
                    call: root,
                    params: 0,
                    args: 1,
                },
                ValidationError::ArgSlotOutOfRange {
                    body: body_id,
                    call: root,
                    slot: ParamIdx::new(0),
                },
            ])
        );
    }

    #[test]
    fn an_unlisted_pattern_reference_is_rejected() {
        let mut program = sample_program();
        let int = program.tys.intern(TyKind::Int);
        let body_id = crate::BodyId::new(0);
        let root = program.bodies[body_id].exprs.push(Expr {
            kind: ExprKind::For {
                pat: PatId::new(4),
                iter: ExprId::new(0),
                body: ExprId::new(0),
            },
            ty: int,
            span: span(2),
            weight: 1,
        });
        program.bodies[body_id].root = root;
        assert_eq!(
            program.validate(),
            Err(vec![ValidationError::PatOutOfRange {
                body: body_id,
                pat: PatId::new(4),
            }])
        );
    }
}
