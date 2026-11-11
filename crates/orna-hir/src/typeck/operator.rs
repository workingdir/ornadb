//! Operator protocols (D16 P1-A): an operator whose left operand is a nominal type
//! and whose kernel scalar row does not apply selects an instance of its `core.ops`
//! protocol. The instance's right protocol argument must equal the right operand's
//! type. The selected implementation member is the call the operator becomes.
//!
//! `in` is the one operator whose receiver is its right operand: `value in container`
//! selects the `Contains` instance of `container` and becomes `container.contains(value)`.

use orna_syntax::SyntaxSpan;

use crate::diag::GENERIC_OVERLAP;
use crate::{
    Arg, ArgValue, BinOp, Body, Call, Callee, DefId, DefKind, ExprId, ExprKind, HirDiagnostic,
    ModulePath, Name, ParamIdx, Ty, TyKind, UnOp,
};

use super::BodyCheck;

/// The `core.ops` protocol and member that serve one operator.
struct OperatorProtocol {
    symbol: &'static str,
    protocol: &'static str,
    member: &'static str,
    /// Whether the protocol's last type argument is the member's result (`Out`).
    output: bool,
}

fn binary_protocol(op: BinOp) -> Option<OperatorProtocol> {
    let (symbol, protocol, member) = match op {
        BinOp::Add => ("+", "Add", "add"),
        BinOp::Sub => ("-", "Sub", "sub"),
        BinOp::Mul => ("*", "Mul", "mul"),
        BinOp::Div => ("/", "Div", "div"),
        BinOp::Rem => ("%", "Rem", "rem"),
        BinOp::Pow => ("^", "Pow", "pow"),
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::In => {
            return None;
        }
    };
    Some(OperatorProtocol {
        symbol,
        protocol,
        member,
        output: true,
    })
}

/// The `Ordering` test an ordering operator lowers to: its symbol, the `==` or `!=` that tests the
/// comparison result, and the `Ordering` variant it compares against.
fn order_test(op: BinOp) -> Option<(&'static str, BinOp, &'static str)> {
    match op {
        BinOp::Lt => Some(("<", BinOp::Eq, "less")),
        BinOp::Le => Some(("<=", BinOp::Ne, "greater")),
        BinOp::Gt => Some((">", BinOp::Eq, "greater")),
        BinOp::Ge => Some((">=", BinOp::Ne, "less")),
        _ => None,
    }
}

/// One instance that can serve an operator: its member and that member's signature.
struct Implementation {
    member: DefId,
    params: Vec<Ty>,
    output: Ty,
    /// The protocol's last type argument, which the member's result must equal.
    protocol_output: Option<Ty>,
}

impl BodyCheck<'_> {
    /// Types `left OP right` through the operator protocol of a nominal left operand.
    ///
    /// `None` means no protocol applies, so the caller keeps the scalar rules.
    /// `Some` is the operator's type, including the error type after a reported
    /// missing or ambiguous instance.
    pub(super) fn binary_operator_ty(
        &mut self,
        body: &mut Body,
        id: ExprId,
        op: BinOp,
        operands: (ExprId, ExprId),
        types: (Ty, Ty),
        span: SyntaxSpan,
    ) -> Option<Ty> {
        if matches!(op, BinOp::In) {
            return self.contains_operator_ty(body, id, operands, types, span);
        }
        if order_test(op).is_some() {
            return self.order_operator_ty(body, id, op, operands, types, span);
        }
        let operator = binary_protocol(op)?;
        let (left_expr, right_expr) = operands;
        let (left, right) = types;
        if matches!(self.kind_of(right), Some(TyKind::Infer(_))) {
            return None;
        }
        let owner = self.nominal_owner(left)?;
        let protocol = self.core_ops_protocol(operator.protocol)?;
        let receiver_args = self.nominal_args(left);
        let found =
            self.operator_implementations(owner, &receiver_args, protocol, &operator, Some(right));
        let Some(implementation) =
            self.settle_operator(found, &operator, (left, Some(right)), &span)
        else {
            return Some(self.error());
        };
        self.unify_operands(&[left, right], &implementation.params, &span);
        body.exprs[id].kind = operator_call(implementation.member, &[left_expr, right_expr], span);
        Some(implementation.output)
    }

    /// Types an ordering comparison of a nominal operand through its `Order` instance.
    ///
    /// `left < right` becomes `left.compare(right) == Ordering.less`; the other ordering operators test the
    /// same `Ordering` result with `==` or `!=`. The `Ordering` nodes are new expressions, so their types are
    /// set here.
    fn order_operator_ty(
        &mut self,
        body: &mut Body,
        id: ExprId,
        op: BinOp,
        operands: (ExprId, ExprId),
        types: (Ty, Ty),
        span: SyntaxSpan,
    ) -> Option<Ty> {
        let (symbol, test, variant) = order_test(op)?;
        let (left_expr, right_expr) = operands;
        let (left, right) = types;
        if matches!(self.kind_of(right), Some(TyKind::Infer(_))) {
            return None;
        }
        let owner = self.nominal_owner(left)?;
        let protocol = self.core_ops_protocol("Order")?;
        let operator = OperatorProtocol {
            symbol,
            protocol: "Order",
            member: "compare",
            output: true,
        };
        let receiver_args = self.nominal_args(left);
        let found = self.operator_implementations(owner, &receiver_args, protocol, &operator, None);
        let Some(implementation) =
            self.settle_operator(found, &operator, (left, Some(right)), &span)
        else {
            return Some(self.error());
        };
        self.unify_operands(&[left, right], &implementation.params, &span);
        let ordering = implementation.output;
        let variant = self.ordering_variant(variant)?;
        let node_span = body.exprs[id].span.clone();
        let call = body.exprs.push(crate::Expr {
            kind: operator_call(implementation.member, &[left_expr, right_expr], span),
            ty: ordering,
            span: node_span.clone(),
            weight: 1,
        });
        let variant = body.exprs.push(crate::Expr {
            kind: ExprKind::Def(variant),
            ty: ordering,
            span: node_span,
            weight: 1,
        });
        body.exprs[id].kind = ExprKind::Binary {
            op: test,
            lhs: call,
            rhs: variant,
        };
        Some(self.program.tys.intern(TyKind::Bool))
    }

    /// The `Ordering` variant named `name`, from the root `core` module.
    fn ordering_variant(&self, name: &str) -> Option<DefId> {
        let path = ModulePath::new([Name::new("core")]);
        let module = self
            .program
            .modules
            .iter()
            .find(|module| module.path == path)?;
        let ordering = *module.exports.get(&Name::new("Ordering"))?;
        let DefKind::Enum(declared) = &self.program.defs[ordering].kind else {
            return None;
        };
        let name = Name::new(name);
        declared
            .variants
            .iter()
            .copied()
            .find(|variant| self.program.defs[*variant].name == name)
    }

    /// Types `value in container` through the `Contains` instance of a nominal container.
    ///
    /// The container is the receiver, so the operator becomes `container.contains(value)`.
    fn contains_operator_ty(
        &mut self,
        body: &mut Body,
        id: ExprId,
        operands: (ExprId, ExprId),
        types: (Ty, Ty),
        span: SyntaxSpan,
    ) -> Option<Ty> {
        let (value_expr, container_expr) = operands;
        let (value, container) = types;
        if matches!(self.kind_of(value), Some(TyKind::Infer(_))) {
            return None;
        }
        let owner = self.nominal_owner(container)?;
        let operator = OperatorProtocol {
            symbol: "in",
            protocol: "Contains",
            member: "contains",
            output: false,
        };
        let protocol = self.core_ops_protocol(operator.protocol)?;
        let receiver_args = self.nominal_args(container);
        let found =
            self.operator_implementations(owner, &receiver_args, protocol, &operator, Some(value));
        let Some(implementation) =
            self.settle_operator(found, &operator, (value, Some(container)), &span)
        else {
            return Some(self.error());
        };
        self.unify_operands(&[container, value], &implementation.params, &span);
        body.exprs[id].kind =
            operator_call(implementation.member, &[container_expr, value_expr], span);
        Some(implementation.output)
    }

    /// Types `-operand` through the `Neg` instance of a nominal operand.
    pub(super) fn unary_operator_ty(
        &mut self,
        body: &mut Body,
        id: ExprId,
        op: UnOp,
        operand: ExprId,
        operand_ty: Ty,
        span: SyntaxSpan,
    ) -> Option<Ty> {
        if !matches!(op, UnOp::Neg) {
            return None;
        }
        let owner = self.nominal_owner(operand_ty)?;
        let operator = OperatorProtocol {
            symbol: "-",
            protocol: "Neg",
            member: "neg",
            output: true,
        };
        let protocol = self.core_ops_protocol(operator.protocol)?;
        let receiver_args = self.nominal_args(operand_ty);
        let found = self.operator_implementations(owner, &receiver_args, protocol, &operator, None);
        let Some(implementation) =
            self.settle_operator(found, &operator, (operand_ty, None), &span)
        else {
            return Some(self.error());
        };
        self.unify_operands(&[operand_ty], &implementation.params, &span);
        body.exprs[id].kind = operator_call(implementation.member, &[operand], span);
        Some(implementation.output)
    }

    /// The nominal owner of a type, when it is a nominal type.
    fn nominal_owner(&self, ty: Ty) -> Option<DefId> {
        match self.kind_of(ty) {
            Some(TyKind::Adt(owner, _)) => Some(owner),
            _ => None,
        }
    }

    /// The `core.ops` protocol called `name`.
    fn core_ops_protocol(&self, name: &str) -> Option<DefId> {
        let path = ModulePath::new([Name::new("core"), Name::new("ops")]);
        let module = self
            .program
            .modules
            .iter()
            .find(|module| module.path == path)?;
        module.exports.get(&Name::new(name)).copied()
    }

    /// The implementations of `protocol` on `owner` that serve the operator.
    ///
    /// `right` filters on the protocol's first type argument: the right operand for
    /// the arithmetic operators, the value for `Contains`.
    fn operator_implementations(
        &mut self,
        owner: DefId,
        receiver_args: &[Ty],
        protocol: DefId,
        operator: &OperatorProtocol,
        right: Option<Ty>,
    ) -> Vec<Implementation> {
        let member_name = Name::new(operator.member);
        let implementations = self
            .program
            .instances
            .get(&owner)
            .cloned()
            .unwrap_or_default();
        let mut found = Vec::new();
        for implementation in implementations {
            let DefKind::Impl(declared) = &self.program.defs[implementation].kind else {
                continue;
            };
            if declared.protocol != Some(protocol) {
                continue;
            }
            let raw_args = declared.protocol_args.clone();
            let members = declared.members.clone();
            let protocol_args: Vec<Ty> = raw_args
                .iter()
                .map(|argument| self.substitute(*argument, receiver_args))
                .collect();
            if let Some(right) = right
                && protocol_args.first() != Some(&right)
            {
                continue;
            }
            let Some(member) = members
                .iter()
                .copied()
                .find(|member| self.program.defs[*member].name == member_name)
            else {
                continue;
            };
            let DefKind::Fn(function) = &self.program.defs[member].kind else {
                continue;
            };
            let declared_params: Vec<Ty> = function.params.iter().map(|param| param.ty).collect();
            let declared_output = function.ret;
            let params = declared_params
                .iter()
                .map(|parameter| self.substitute(*parameter, receiver_args))
                .collect();
            let output = self.substitute(declared_output, receiver_args);
            found.push(Implementation {
                member,
                params,
                output,
                protocol_output: protocol_args.last().copied().filter(|_| operator.output),
            });
        }
        found
    }

    /// Picks the single implementation an operator uses, reporting a missing or ambiguous one.
    fn settle_operator(
        &mut self,
        mut found: Vec<Implementation>,
        operator: &OperatorProtocol,
        operands: (Ty, Option<Ty>),
        span: &SyntaxSpan,
    ) -> Option<Implementation> {
        let (left, right) = operands;
        match found.len() {
            0 => {
                let shown = match right {
                    Some(right) => format!(
                        "{} and {}",
                        crate::print::print_type(self.program, left),
                        crate::print::print_type(self.program, right),
                    ),
                    None => crate::print::print_type(self.program, left),
                };
                self.conflict(
                    format!(
                        "operator `{}` is not defined for {shown}: no `{}` instance takes that operand",
                        operator.symbol, operator.protocol,
                    ),
                    span.clone(),
                );
                None
            }
            1 => {
                let implementation = found.remove(0);
                if let Some(expected) = implementation.protocol_output {
                    self.unify(implementation.output, expected, span.clone());
                }
                Some(implementation)
            }
            _ => {
                self.conflicts += 1;
                self.diagnostics.push(HirDiagnostic::new(
                    GENERIC_OVERLAP,
                    format!(
                        "two `{}` instances supply the operator `{}` on {}",
                        operator.protocol,
                        operator.symbol,
                        crate::print::print_type(self.program, left),
                    ),
                    span.clone(),
                ));
                None
            }
        }
    }

    /// Unifies each operand with the parameter it binds.
    fn unify_operands(&mut self, operands: &[Ty], params: &[Ty], span: &SyntaxSpan) {
        for (operand, param) in operands.iter().zip(params) {
            self.unify(*operand, *param, span.clone());
        }
    }
}

/// The source call an operator becomes once its implementation is selected.
fn operator_call(member: DefId, operands: &[ExprId], span: SyntaxSpan) -> ExprKind {
    let args = operands
        .iter()
        .enumerate()
        .map(|(slot, operand)| Arg {
            slot: ParamIdx::new(slot as u32),
            value: ArgValue::Expr(*operand),
        })
        .collect();
    ExprKind::Call(Box::new(Call {
        callee: Callee::Source {
            def: member,
            targs: Box::new([]),
        },
        callee_span: span,
        args,
        piped: false,
    }))
}
