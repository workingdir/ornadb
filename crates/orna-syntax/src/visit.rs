//! Exhaustive traversal of the syntax tree.
//!
//! Every consumer that needs to walk the whole tree (the editor's span
//! collectors, call-site search, declaration body extraction, name analysis)
//! shares this one enumeration of each node's children. A new AST variant is
//! added here once; the `walk_*` functions match without catch-all arms, so a
//! variant the walk forgets fails to compile instead of being silently
//! skipped.
//!
//! [`Visit::visit_span`] is called for every source range the tree carries,
//! including the ranges of syntax that is not itself a node (an argument, a
//! record field, a constructor pattern's path segments). Implementations that
//! only need structure leave it empty and override the typed hooks they care
//! about; the default of a typed hook walks its children.

use crate::parser::{
    Argument, AssignmentTarget, CaseArm, ControlKind, Declaration, DimensionExpr, EnumPayloadField,
    EnumVariant, Expr, FieldInitializer, FunctionSignature, GenericParameter, ImplMember,
    Implementation, ImportSegment, Item, LambdaParameter, NameSegment, Parameter, Pattern,
    PatternField, ProtocolMember, RecordField, SourceSpan, Statement, StringSegment, SyntaxTree,
    TableMember, TypeExpr, TypeMember, TypeRepresentation, UnitDefinition, UseTail, Visibility,
};

/// A whole-tree traversal with hooks for each node kind.
pub trait Visit {
    /// The source range of any tree element, visited once per element.
    fn visit_span(&mut self, _span: &SourceSpan) {}

    fn visit_tree(&mut self, tree: &SyntaxTree) {
        walk_tree(self, tree);
    }
    fn visit_item(&mut self, item: &Item) {
        walk_item(self, item);
    }
    fn visit_visibility(&mut self, visibility: &Visibility) {
        walk_visibility(self, visibility);
    }
    fn visit_declaration(&mut self, declaration: &Declaration) {
        walk_declaration(self, declaration);
    }
    fn visit_import_segment(&mut self, segment: &ImportSegment) {
        walk_import_segment(self, segment);
    }
    fn visit_use_tail(&mut self, tail: &UseTail) {
        walk_use_tail(self, tail);
    }
    fn visit_generic_parameter(&mut self, parameter: &GenericParameter) {
        walk_generic_parameter(self, parameter);
    }
    fn visit_parameter(&mut self, parameter: &Parameter) {
        walk_parameter(self, parameter);
    }
    fn visit_signature(&mut self, signature: &FunctionSignature) {
        walk_signature(self, signature);
    }
    fn visit_table_member(&mut self, member: &TableMember) {
        walk_table_member(self, member);
    }
    fn visit_protocol_member(&mut self, member: &ProtocolMember) {
        walk_protocol_member(self, member);
    }
    fn visit_enum_variant(&mut self, variant: &EnumVariant) {
        walk_enum_variant(self, variant);
    }
    fn visit_enum_field(&mut self, field: &EnumPayloadField) {
        walk_enum_field(self, field);
    }
    fn visit_type_member(&mut self, member: &TypeMember) {
        walk_type_member(self, member);
    }
    fn visit_type_representation(&mut self, representation: &TypeRepresentation) {
        walk_type_representation(self, representation);
    }
    fn visit_implementation(&mut self, implementation: &Implementation) {
        walk_implementation(self, implementation);
    }
    fn visit_impl_member(&mut self, member: &ImplMember) {
        walk_impl_member(self, member);
    }
    fn visit_dimension(&mut self, expression: &DimensionExpr) {
        walk_dimension(self, expression);
    }
    fn visit_unit_definition(&mut self, definition: &UnitDefinition) {
        walk_unit_definition(self, definition);
    }
    fn visit_type(&mut self, ty: &TypeExpr) {
        walk_type(self, ty);
    }
    fn visit_expr(&mut self, expression: &Expr) {
        walk_expr(self, expression);
    }
    fn visit_statement(&mut self, statement: &Statement) {
        walk_statement(self, statement);
    }
    fn visit_pattern(&mut self, pattern: &Pattern) {
        walk_pattern(self, pattern);
    }
    fn visit_argument(&mut self, argument: &Argument) {
        walk_argument(self, argument);
    }
    fn visit_record_field(&mut self, field: &RecordField) {
        walk_record_field(self, field);
    }
    fn visit_lambda_parameter(&mut self, parameter: &LambdaParameter) {
        walk_lambda_parameter(self, parameter);
    }
    fn visit_case_arm(&mut self, arm: &CaseArm) {
        walk_case_arm(self, arm);
    }
    fn visit_control_binding(&mut self, pattern: &Pattern) {
        walk_control_binding(self, pattern);
    }
    /// The `case` arms of a control expression. A `for` loop also stores a
    /// copy of its body as one arm (A01-17); visitors that must not see the
    /// body twice override this to skip that copy.
    fn visit_control_arms(&mut self, kind: ControlKind, arms: &[CaseArm]) {
        walk_control_arms(self, kind, arms);
    }
    fn visit_name_segment(&mut self, segment: &NameSegment) {
        walk_name_segment(self, segment);
    }
    fn visit_string_segment(&mut self, segment: &StringSegment) {
        walk_string_segment(self, segment);
    }
    fn visit_assignment_target(&mut self, target: &AssignmentTarget) {
        walk_assignment_target(self, target);
    }
    fn visit_pattern_field(&mut self, field: &PatternField) {
        walk_pattern_field(self, field);
    }
    /// The `name` range of a field access or assignment target.
    fn visit_field_name(&mut self, span: &SourceSpan) {
        self.visit_span(span);
    }
}

/// Visits every child of `tree`.
pub fn walk_tree<V: Visit + ?Sized>(visitor: &mut V, tree: &SyntaxTree) {
    for item in &tree.items {
        visitor.visit_item(item);
    }
}

/// Visits every child of `item`.
pub fn walk_item<V: Visit + ?Sized>(visitor: &mut V, item: &Item) {
    visitor.visit_span(&item.span);
    visitor.visit_visibility(&item.visibility);
    visitor.visit_declaration(&item.declaration);
}

/// Visits the span of a public visibility modifier.
pub fn walk_visibility<V: Visit + ?Sized>(visitor: &mut V, visibility: &Visibility) {
    if let Visibility::Public { span } = visibility {
        visitor.visit_span(span);
    }
}

/// Visits every child of `declaration`.
pub fn walk_declaration<V: Visit + ?Sized>(visitor: &mut V, declaration: &Declaration) {
    match declaration {
        Declaration::Use { path, tail } => {
            for segment in path {
                visitor.visit_import_segment(segment);
            }
            visitor.visit_use_tail(tail);
        }
        Declaration::Table { keys, members, .. } => {
            for key in keys {
                visitor.visit_parameter(key);
            }
            for member in members {
                visitor.visit_table_member(member);
            }
        }
        Declaration::Function { signature, body } => {
            visitor.visit_signature(signature);
            visitor.visit_expr(body);
        }
        Declaration::Protocol {
            generics, members, ..
        } => {
            for generic in generics {
                visitor.visit_generic_parameter(generic);
            }
            for member in members {
                visitor.visit_protocol_member(member);
            }
        }
        Declaration::Enum {
            generics, variants, ..
        } => {
            for generic in generics {
                visitor.visit_generic_parameter(generic);
            }
            for variant in variants {
                visitor.visit_enum_variant(variant);
            }
        }
        Declaration::Dimension { expression, .. } => {
            if let Some(expression) = expression {
                visitor.visit_dimension(expression);
            }
        }
        Declaration::Unit {
            dimension,
            definition,
            ..
        } => {
            visitor.visit_type(dimension);
            visitor.visit_unit_definition(definition);
        }
        Declaration::Type {
            generics,
            representation,
            ..
        } => {
            for generic in generics {
                visitor.visit_generic_parameter(generic);
            }
            visitor.visit_type_representation(representation);
        }
        Declaration::Assertion { value } => visitor.visit_expr(value),
        Declaration::Let {
            pattern,
            annotation,
            value,
        } => {
            visitor.visit_pattern(pattern);
            if let Some(annotation) = annotation {
                visitor.visit_type(annotation);
            }
            visitor.visit_expr(value);
        }
    }
}

/// Visits the span of one import path segment.
pub fn walk_import_segment<V: Visit + ?Sized>(visitor: &mut V, segment: &ImportSegment) {
    visitor.visit_span(&segment.span);
}

/// Visits the tail of a `use` declaration.
pub fn walk_use_tail<V: Visit + ?Sized>(visitor: &mut V, tail: &UseTail) {
    match tail {
        UseTail::None => {}
        UseTail::Alias { span, .. } | UseTail::Glob { span } => visitor.visit_span(span),
        UseTail::Names(names) => {
            for name in names {
                visitor.visit_import_segment(name);
            }
        }
    }
}

/// Visits the bounds and span of a generic parameter.
pub fn walk_generic_parameter<V: Visit + ?Sized>(visitor: &mut V, parameter: &GenericParameter) {
    for bound in &parameter.bounds {
        visitor.visit_type(bound);
    }
    visitor.visit_span(&parameter.span);
}

/// Visits the pattern, annotation, default and span of a parameter.
pub fn walk_parameter<V: Visit + ?Sized>(visitor: &mut V, parameter: &Parameter) {
    visitor.visit_pattern(&parameter.pattern);
    if let Some(annotation) = &parameter.annotation {
        visitor.visit_type(annotation);
    }
    if let Some(default) = &parameter.default {
        visitor.visit_expr(default);
    }
    visitor.visit_span(&parameter.span);
}

/// Visits the generics, parameters, result and span of a signature.
pub fn walk_signature<V: Visit + ?Sized>(visitor: &mut V, signature: &FunctionSignature) {
    for generic in &signature.generics {
        visitor.visit_generic_parameter(generic);
    }
    for parameter in &signature.parameters {
        visitor.visit_parameter(parameter);
    }
    if let Some(result) = &signature.result {
        visitor.visit_type(result);
    }
    visitor.visit_span(&signature.span);
}

/// Visits every child of a table member.
pub fn walk_table_member<V: Visit + ?Sized>(visitor: &mut V, member: &TableMember) {
    match member {
        TableMember::Field {
            ty,
            initializer,
            span,
            ..
        } => {
            visitor.visit_span(span);
            visitor.visit_type(ty);
            if let Some(initializer) = initializer {
                match initializer {
                    FieldInitializer::Default(value) | FieldInitializer::Computed(value) => {
                        visitor.visit_expr(value)
                    }
                }
            }
        }
        TableMember::Assertion { value, span } => {
            visitor.visit_span(span);
            visitor.visit_expr(value);
        }
        TableMember::Implementation {
            implementation,
            span,
        } => {
            visitor.visit_span(span);
            visitor.visit_implementation(implementation);
        }
    }
}

/// Visits every child of a protocol member.
pub fn walk_protocol_member<V: Visit + ?Sized>(visitor: &mut V, member: &ProtocolMember) {
    match member {
        ProtocolMember::Function { signature, span } => {
            visitor.visit_span(span);
            visitor.visit_signature(signature);
        }
        ProtocolMember::Static { ty, span, .. } => {
            visitor.visit_span(span);
            visitor.visit_type(ty);
        }
    }
}

/// Visits the fields and span of an enum variant.
pub fn walk_enum_variant<V: Visit + ?Sized>(visitor: &mut V, variant: &EnumVariant) {
    visitor.visit_span(&variant.span);
    for field in &variant.fields {
        visitor.visit_enum_field(field);
    }
}

/// Visits the type and span of an enum payload field.
pub fn walk_enum_field<V: Visit + ?Sized>(visitor: &mut V, field: &EnumPayloadField) {
    visitor.visit_span(&field.span);
    visitor.visit_type(&field.ty);
}

/// Visits every child of a type member.
pub fn walk_type_member<V: Visit + ?Sized>(visitor: &mut V, member: &TypeMember) {
    match member {
        TypeMember::Field {
            ty,
            initializer,
            span,
            ..
        } => {
            visitor.visit_span(span);
            visitor.visit_type(ty);
            if let Some(initializer) = initializer {
                visitor.visit_expr(initializer);
            }
        }
        TypeMember::Assertion { value, span } => {
            visitor.visit_span(span);
            visitor.visit_expr(value);
        }
        TypeMember::Implementation {
            implementation,
            span,
        } => {
            visitor.visit_span(span);
            visitor.visit_implementation(implementation);
        }
    }
}

/// Visits every child of a type representation.
pub fn walk_type_representation<V: Visit + ?Sized>(
    visitor: &mut V,
    representation: &TypeRepresentation,
) {
    match representation {
        TypeRepresentation::Alias { ty, refinements } => {
            visitor.visit_type(ty);
            for member in refinements {
                visitor.visit_type_member(member);
            }
        }
        TypeRepresentation::Nominal { members } => {
            for member in members {
                visitor.visit_type_member(member);
            }
        }
    }
}

/// Visits the protocol type, members and span of an implementation.
pub fn walk_implementation<V: Visit + ?Sized>(visitor: &mut V, implementation: &Implementation) {
    visitor.visit_span(&implementation.span);
    visitor.visit_type(&implementation.protocol);
    for member in &implementation.members {
        visitor.visit_impl_member(member);
    }
}

/// Visits every child of an implementation member.
pub fn walk_impl_member<V: Visit + ?Sized>(visitor: &mut V, member: &ImplMember) {
    match member {
        ImplMember::Function {
            signature,
            body,
            span,
        } => {
            visitor.visit_span(span);
            visitor.visit_signature(signature);
            visitor.visit_expr(body);
        }
        ImplMember::Static {
            ty, value, span, ..
        } => {
            visitor.visit_span(span);
            if let Some(ty) = ty {
                visitor.visit_type(ty);
            }
            visitor.visit_expr(value);
        }
    }
}

/// Visits the terms and span of a dimension expression.
pub fn walk_dimension<V: Visit + ?Sized>(visitor: &mut V, expression: &DimensionExpr) {
    for (_, ty, _, span) in &expression.terms {
        visitor.visit_span(span);
        visitor.visit_type(ty);
    }
    visitor.visit_span(&expression.span);
}

/// Visits the values of a unit definition.
pub fn walk_unit_definition<V: Visit + ?Sized>(visitor: &mut V, definition: &UnitDefinition) {
    if let UnitDefinition::Derived { value, offset, .. } = definition {
        visitor.visit_expr(value);
        if let Some(offset) = offset {
            visitor.visit_expr(offset);
        }
    }
}

/// Visits every child of a type expression.
pub fn walk_type<V: Visit + ?Sized>(visitor: &mut V, ty: &TypeExpr) {
    match ty {
        TypeExpr::Name {
            arguments, span, ..
        } => {
            visitor.visit_span(span);
            for argument in arguments {
                visitor.visit_type(argument);
            }
        }
        TypeExpr::Optional { inner, span } | TypeExpr::List { inner, span } => {
            visitor.visit_span(span);
            visitor.visit_type(inner);
        }
        TypeExpr::Product { lhs, rhs, span, .. } => {
            visitor.visit_span(span);
            visitor.visit_type(lhs);
            visitor.visit_type(rhs);
        }
        TypeExpr::Record { fields, span } => {
            visitor.visit_span(span);
            for (_, ty, field_span) in fields {
                visitor.visit_span(field_span);
                visitor.visit_type(ty);
            }
        }
        TypeExpr::Tuple { elements, span } => {
            visitor.visit_span(span);
            for element in elements {
                visitor.visit_type(element);
            }
        }
        TypeExpr::Function {
            parameters,
            result,
            span,
        } => {
            visitor.visit_span(span);
            for parameter in parameters {
                visitor.visit_type(parameter);
            }
            visitor.visit_type(result);
        }
    }
}

/// Visits every child of an expression.
pub fn walk_expr<V: Visit + ?Sized>(visitor: &mut V, expression: &Expr) {
    match expression {
        Expr::Name { span, .. } | Expr::Literal { span, .. } | Expr::ReplBinding { span, .. } => {
            visitor.visit_span(span)
        }
        Expr::InterpolatedString { segments, span } => {
            visitor.visit_span(span);
            for segment in segments {
                visitor.visit_string_segment(segment);
            }
        }
        Expr::Unary { rhs, span, .. } => {
            visitor.visit_span(span);
            visitor.visit_expr(rhs);
        }
        Expr::Binary { lhs, rhs, span, .. } => {
            visitor.visit_span(span);
            visitor.visit_expr(lhs);
            visitor.visit_expr(rhs);
        }
        Expr::Range {
            lower, upper, span, ..
        } => {
            visitor.visit_span(span);
            if let Some(lower) = lower {
                visitor.visit_expr(lower);
            }
            if let Some(upper) = upper {
                visitor.visit_expr(upper);
            }
        }
        Expr::Call { .. } | Expr::GenericCall { .. } => walk_call(visitor, expression),
        Expr::Index { base, index, span } => {
            visitor.visit_span(span);
            visitor.visit_expr(base);
            visitor.visit_expr(index);
        }
        Expr::Field { base, name, span } => {
            visitor.visit_span(span);
            visitor.visit_field_name(&SourceSpan::new(
                span.end.saturating_sub(name.len()),
                span.end,
            ));
            visitor.visit_expr(base);
        }
        Expr::Group { inner, span } => {
            visitor.visit_span(span);
            visitor.visit_expr(inner);
        }
        Expr::Tuple { elements, span } | Expr::List { elements, span } => {
            visitor.visit_span(span);
            for element in elements {
                visitor.visit_expr(element);
            }
        }
        Expr::Record { .. } | Expr::Nominal { .. } => walk_record(visitor, expression),
        Expr::Lambda {
            parameters,
            body,
            span,
        } => {
            visitor.visit_span(span);
            for parameter in parameters {
                visitor.visit_lambda_parameter(parameter);
            }
            visitor.visit_expr(body);
        }
        Expr::Block {
            statements,
            tail,
            span,
        } => {
            visitor.visit_span(span);
            for statement in statements {
                visitor.visit_statement(statement);
            }
            if let Some(tail) = tail {
                visitor.visit_expr(tail);
            }
        }
        Expr::Control { .. } => walk_control(visitor, expression),
    }
}

/// Visits a call expression: the callee, its type arguments and its argument
/// values. Called only for `Expr::Call` and `Expr::GenericCall`.
fn walk_call<V: Visit + ?Sized>(visitor: &mut V, expression: &Expr) {
    let (callee, type_arguments, arguments, span) = match expression {
        Expr::Call {
            callee,
            arguments,
            span,
        } => (callee, &[][..], arguments, span),
        Expr::GenericCall {
            callee,
            type_arguments,
            arguments,
            span,
        } => (callee, &type_arguments[..], arguments, span),
        _ => unreachable!("walk_call is only called for call expressions"),
    };
    visitor.visit_span(span);
    visitor.visit_expr(callee);
    for ty in type_arguments {
        visitor.visit_type(ty);
    }
    for argument in arguments {
        visitor.visit_argument(argument);
    }
}

/// Visits a record literal or nominal constructor. Called only for
/// `Expr::Record` and `Expr::Nominal`.
fn walk_record<V: Visit + ?Sized>(visitor: &mut V, expression: &Expr) {
    let (path, fields, span) = match expression {
        Expr::Record { fields, span } => (None, fields, span),
        Expr::Nominal { path, fields, span } => (Some(path.as_slice()), fields, span),
        _ => unreachable!("walk_record is only called for record expressions"),
    };
    visitor.visit_span(span);
    if let Some(path) = path {
        for segment in path {
            visitor.visit_name_segment(segment);
        }
    }
    for field in fields {
        visitor.visit_record_field(field);
    }
}

/// Visits a control expression: binding, condition, body, arms and alternate.
/// Called only for `Expr::Control`.
fn walk_control<V: Visit + ?Sized>(visitor: &mut V, expression: &Expr) {
    let Expr::Control {
        kind,
        binding,
        condition,
        body,
        arms,
        alternate,
        span,
        ..
    } = expression
    else {
        unreachable!("walk_control is only called for control expressions");
    };
    visitor.visit_span(span);
    if let Some(binding) = binding {
        visitor.visit_control_binding(binding);
    }
    if let Some(condition) = condition {
        visitor.visit_expr(condition);
    }
    if let Some(body) = body {
        visitor.visit_expr(body);
    }
    visitor.visit_control_arms(*kind, arms);
    if let Some(alternate) = alternate {
        visitor.visit_expr(alternate);
    }
}

/// Visits every child of a statement.
pub fn walk_statement<V: Visit + ?Sized>(visitor: &mut V, statement: &Statement) {
    match statement {
        Statement::Let {
            pattern,
            annotation,
            value,
            span,
        } => {
            visitor.visit_span(span);
            visitor.visit_pattern(pattern);
            if let Some(annotation) = annotation {
                visitor.visit_type(annotation);
            }
            visitor.visit_expr(value);
        }
        Statement::Assignment {
            target,
            value,
            span,
            ..
        } => {
            visitor.visit_span(span);
            visitor.visit_assignment_target(target);
            visitor.visit_expr(value);
        }
        Statement::Assert { value, span }
        | Statement::Expression { value, span }
        | Statement::Control { value, span } => {
            visitor.visit_span(span);
            visitor.visit_expr(value);
        }
        Statement::Return { value, span } | Statement::Break { value, span } => {
            visitor.visit_span(span);
            if let Some(value) = value {
                visitor.visit_expr(value);
            }
        }
        Statement::Continue { span } => visitor.visit_span(span),
    }
}

/// Visits every child of a pattern.
pub fn walk_pattern<V: Visit + ?Sized>(visitor: &mut V, pattern: &Pattern) {
    match pattern {
        Pattern::Name(_, span) | Pattern::Wildcard(span) | Pattern::Literal { span, .. } => {
            visitor.visit_span(span)
        }
        Pattern::Tuple { elements, span } | Pattern::List { elements, span } => {
            visitor.visit_span(span);
            for element in elements {
                visitor.visit_pattern(element);
            }
        }
        Pattern::Record { fields, span } => {
            visitor.visit_span(span);
            for (_, pattern, field_span) in fields {
                visitor.visit_span(field_span);
                if let Some(pattern) = pattern {
                    visitor.visit_pattern(pattern);
                }
            }
        }
        Pattern::Constructor {
            path,
            arguments,
            fields,
            span,
        } => {
            visitor.visit_span(span);
            for segment in path {
                visitor.visit_name_segment(segment);
            }
            for argument in arguments {
                visitor.visit_pattern(argument);
            }
            for field in fields {
                visitor.visit_pattern_field(field);
            }
        }
    }
}

/// Visits the span and value of a call argument.
pub fn walk_argument<V: Visit + ?Sized>(visitor: &mut V, argument: &Argument) {
    visitor.visit_span(&argument.span);
    visitor.visit_expr(&argument.value);
}

/// Visits the span and value of a record field.
pub fn walk_record_field<V: Visit + ?Sized>(visitor: &mut V, field: &RecordField) {
    visitor.visit_span(&field.span);
    visitor.visit_expr(&field.value);
}

/// Visits the span, pattern and annotation of a lambda parameter.
pub fn walk_lambda_parameter<V: Visit + ?Sized>(visitor: &mut V, parameter: &LambdaParameter) {
    visitor.visit_span(&parameter.span);
    visitor.visit_pattern(&parameter.pattern);
    if let Some(annotation) = &parameter.annotation {
        visitor.visit_type(annotation);
    }
}

/// Visits the span, pattern, guard and body of a case arm.
pub fn walk_case_arm<V: Visit + ?Sized>(visitor: &mut V, arm: &CaseArm) {
    visitor.visit_span(&arm.span);
    visitor.visit_pattern(&arm.pattern);
    if let Some(guard) = &arm.guard {
        visitor.visit_expr(guard);
    }
    visitor.visit_expr(&arm.body);
}

/// Visits the pattern a `for` loop binds.
pub fn walk_control_binding<V: Visit + ?Sized>(visitor: &mut V, pattern: &Pattern) {
    visitor.visit_pattern(pattern);
}

/// Visits every case arm, including the copy of a `for` body stored as one.
pub fn walk_control_arms<V: Visit + ?Sized>(visitor: &mut V, _kind: ControlKind, arms: &[CaseArm]) {
    for arm in arms {
        visitor.visit_case_arm(arm);
    }
}

/// Visits the span of one name path segment.
pub fn walk_name_segment<V: Visit + ?Sized>(visitor: &mut V, segment: &NameSegment) {
    visitor.visit_span(&segment.span);
}

/// Visits the span and expression of a string segment.
pub fn walk_string_segment<V: Visit + ?Sized>(visitor: &mut V, segment: &StringSegment) {
    match segment {
        StringSegment::Text { span, .. } => visitor.visit_span(span),
        StringSegment::Expression { value, span } => {
            visitor.visit_span(span);
            visitor.visit_expr(value);
        }
    }
}

/// Visits every child of an assignment target.
pub fn walk_assignment_target<V: Visit + ?Sized>(visitor: &mut V, target: &AssignmentTarget) {
    match target {
        AssignmentTarget::Name { span, .. } => visitor.visit_span(span),
        AssignmentTarget::Field { base, name, span } => {
            visitor.visit_span(span);
            visitor.visit_field_name(&SourceSpan::new(
                span.end.saturating_sub(name.len()),
                span.end,
            ));
            visitor.visit_assignment_target(base);
        }
        AssignmentTarget::Index { base, index, span } => {
            visitor.visit_span(span);
            visitor.visit_assignment_target(base);
            visitor.visit_expr(index);
        }
    }
}

/// Visits the span and pattern of a constructor pattern field.
pub fn walk_pattern_field<V: Visit + ?Sized>(visitor: &mut V, field: &PatternField) {
    visitor.visit_span(&field.span);
    if let Some(pattern) = &field.pattern {
        visitor.visit_pattern(pattern);
    }
}
