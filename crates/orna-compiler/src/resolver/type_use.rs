//! Standard-application type-use recording.
//!
//! This module owns standard filtering and expression ordinal progression so
//! resolver branches only describe the checked source shape they accepted.

use orna_core::TypeId;
use orna_syntax::{
    DeleteStatement, InsertStatement, QueryExpression, SelectQuery, SourceSpan, UpdateStatement,
};

use crate::{
    SourceLocation,
    mutation::{DeleteCheck, MutationCheck, MutationExpressionUse},
    relational::{ExpressionIr, ExpressionKind, OrderingIr},
};

use super::{
    CheckedApplicationTypeUse, CheckedFieldId, CheckedFunctionId, CheckedObjectReferenceUse,
    CheckedParameterId, CheckedStandardLibrary, CheckedTypeId, CheckedTypeUseKind,
    CheckedValueTypeUse, ResolvedApplicationType, SemanticType, location,
};

type CheckedMutation =
    MutationCheck<CheckedTypeId, CheckedFieldId, CheckedFunctionId, CheckedParameterId>;
type CheckedDelete =
    DeleteCheck<CheckedTypeId, CheckedFieldId, CheckedFunctionId, CheckedParameterId>;
pub(super) fn record_standard_type_use(
    uses: &mut Vec<CheckedApplicationTypeUse>,
    standard: Option<&CheckedStandardLibrary>,
    kind: CheckedTypeUseKind,
    resolved: ResolvedApplicationType,
    source_location: SourceLocation,
) {
    record_resolved_type_use(uses, standard.is_some(), kind, resolved, source_location);
}

/// Records the ordered type evidence emitted by one checked function body.
///
/// The recorder is intentionally stateful: callers cannot manufacture or
/// accidentally reuse expression ordinals, and legacy checks avoid traversing
/// accepted expression trees when no verified standard library is present.
pub(super) struct StandardTypeUseRecorder<'a, 'b> {
    uses: &'a mut Vec<CheckedApplicationTypeUse>,
    enabled: bool,
    owner: CheckedFunctionId,
    logical_path: &'b str,
    next_expression_ordinal: u32,
}

impl<'a, 'b> StandardTypeUseRecorder<'a, 'b> {
    pub(super) fn new(
        uses: &'a mut Vec<CheckedApplicationTypeUse>,
        standard: Option<&CheckedStandardLibrary>,
        owner: CheckedFunctionId,
        logical_path: &'b str,
    ) -> Self {
        Self {
            uses,
            enabled: standard.is_some(),
            owner,
            logical_path,
            next_expression_ordinal: 0,
        }
    }

    pub(super) fn record_client_body(
        &mut self,
        resolved: ResolvedApplicationType,
        source_location: SourceLocation,
    ) {
        if !self.enabled {
            return;
        }
        let expression = self.next_expression_kind();
        record_resolved_type_use(
            self.uses,
            true,
            expression,
            resolved,
            source_location.clone(),
        );
        record_resolved_type_use(
            self.uses,
            true,
            self.result_kind(0),
            resolved,
            source_location,
        );
    }

    pub(super) fn record_query_body(
        &mut self,
        query: &SelectQuery,
        projections: &[ExpressionIr<CheckedTypeId, CheckedFieldId>],
        selection: Option<&ExpressionIr<CheckedTypeId, CheckedFieldId>>,
        ordering: &[orna_syntax::OrderingExpression],
        checked_ordering: &[OrderingIr<CheckedTypeId, CheckedFieldId>],
    ) {
        if !self.enabled {
            return;
        }
        for (result_ordinal, (source, expression)) in
            query.projections.iter().zip(projections).enumerate()
        {
            self.record_query_expression(source, expression);
            self.record_expression_use(
                self.result_kind(result_ordinal as u32),
                expression,
                location(self.logical_path, source.span()),
            );
        }
        if let (Some(source), Some(expression)) = (query.predicate.as_ref(), selection) {
            self.record_query_expression(source, expression);
        }
        for (source, ordering) in ordering.iter().zip(checked_ordering) {
            self.record_query_expression(&source.expression, ordering.expression());
        }
    }

    pub(super) fn record_identity_selector(
        &mut self,
        query: &SelectQuery,
        target: CheckedTypeId,
        boolean_type: Option<TypeId>,
    ) {
        if !self.enabled {
            return;
        }
        let Some(QueryExpression::Equality { left, right, .. }) = query.predicate.as_ref() else {
            return;
        };
        self.record_value_span(
            query
                .predicate
                .as_ref()
                .map_or(&query.span, |value| value.span()),
            boolean_type,
        );
        self.record_object_span(left.span(), target);
        self.record_object_span(right.span(), target);
    }

    pub(super) fn record_unique_text_selector(
        &mut self,
        query: &SelectQuery,
        boolean_type: Option<TypeId>,
        text_type: Option<TypeId>,
    ) {
        if !self.enabled {
            return;
        }
        let Some(QueryExpression::Equality { left, right, .. }) = query.predicate.as_ref() else {
            return;
        };
        self.record_value_span(
            query
                .predicate
                .as_ref()
                .map_or(&query.span, |value| value.span()),
            boolean_type,
        );
        self.record_value_span(left.span(), text_type);
        self.record_value_span(right.span(), text_type);
    }

    pub(super) fn record_delete(
        &mut self,
        source: &DeleteStatement,
        checked: &CheckedDelete,
        boolean_type: Option<TypeId>,
    ) {
        if !self.enabled {
            return;
        }
        let target = checked.plan().target_object();
        self.record_value_span(&source.selector_equality_span, boolean_type);
        self.record_object_span(&source.selector_ref_span, target);
        self.record_object_span(&source.selector_parameter.span, target);
        self.record_value_span(&source.returning_true.span, boolean_type);
        self.record_value_result(&source.returning_true.span, boolean_type, 0);
    }

    pub(super) fn record_insert(&mut self, source: &InsertStatement, checked: &CheckedMutation) {
        if !self.enabled {
            return;
        }
        for expression in checked.expression_uses() {
            self.record_mutation_expression(expression);
        }
        let returned = checked.plan().returned_object();
        self.record_object_span(&source.returning_ref_span, returned);
        self.record_object_result(&source.returning_ref_span, returned, 0);
    }

    pub(super) fn record_update(
        &mut self,
        source: &UpdateStatement,
        checked: &CheckedMutation,
        boolean_type: Option<TypeId>,
    ) {
        if !self.enabled {
            return;
        }
        for expression in checked.expression_uses() {
            self.record_mutation_expression(expression);
        }
        let plan = checked.plan();
        self.record_value_span(&source.selector_equality_span, boolean_type);
        self.record_object_span(&source.selector_ref_span, plan.target_object());
        self.record_object_span(&source.selector_parameter.span, plan.target_object());
        self.record_object_span(&source.returning_ref_span, plan.returned_object());
        self.record_object_result(&source.returning_ref_span, plan.returned_object(), 0);
    }

    fn record_query_expression(
        &mut self,
        source: &QueryExpression,
        expression: &ExpressionIr<CheckedTypeId, CheckedFieldId>,
    ) {
        let kind = self.next_expression_kind();
        self.record_expression_use(kind, expression, location(self.logical_path, source.span()));

        if let (
            QueryExpression::Equality {
                left: source_left,
                right: source_right,
                ..
            },
            ExpressionKind::Equality {
                left: checked_left,
                right: checked_right,
            },
        ) = (source, expression.kind())
        {
            self.record_query_expression(source_left, checked_left);
            self.record_query_expression(source_right, checked_right);
        }
    }

    fn record_mutation_expression(&mut self, expression: &MutationExpressionUse<CheckedTypeId>) {
        let kind = self.next_expression_kind();
        if let Some(type_id) = expression.value_type().standard_value_type() {
            self.record_value_type(kind, Some(type_id), expression.location().clone());
        } else if let SemanticType::Reference { target } = expression.value_type().semantic_type() {
            self.record_object_reference(kind, target, expression.location().clone());
        }
    }

    fn record_value_span(&mut self, span: &SourceSpan, type_id: Option<TypeId>) {
        let kind = self.next_expression_kind();
        self.record_value_type(kind, type_id, location(self.logical_path, span));
    }

    fn record_object_span(&mut self, span: &SourceSpan, target: CheckedTypeId) {
        let kind = self.next_expression_kind();
        self.record_object_reference(kind, target, location(self.logical_path, span));
    }

    fn record_object_result(&mut self, span: &SourceSpan, target: CheckedTypeId, ordinal: u32) {
        self.record_object_reference(
            self.result_kind(ordinal),
            target,
            location(self.logical_path, span),
        );
    }

    fn record_value_result(&mut self, span: &SourceSpan, type_id: Option<TypeId>, ordinal: u32) {
        self.record_value_type(
            self.result_kind(ordinal),
            type_id,
            location(self.logical_path, span),
        );
    }

    fn record_expression_use(
        &mut self,
        kind: CheckedTypeUseKind,
        expression: &ExpressionIr<CheckedTypeId, CheckedFieldId>,
        source_location: SourceLocation,
    ) {
        if let Some(type_id) = expression.value_type().standard_value_type() {
            self.record_value_type(kind, Some(type_id), source_location);
        } else if let SemanticType::Reference { target } = expression.value_type().semantic_type() {
            self.record_object_reference(kind, target, source_location);
        }
    }

    fn record_value_type(
        &mut self,
        kind: CheckedTypeUseKind,
        type_id: Option<TypeId>,
        source_location: SourceLocation,
    ) {
        if let Some(type_id) = type_id {
            self.uses
                .push(CheckedApplicationTypeUse::Value(CheckedValueTypeUse {
                    type_id,
                    kind,
                    location: source_location,
                }));
        }
    }

    fn record_object_reference(
        &mut self,
        kind: CheckedTypeUseKind,
        target: CheckedTypeId,
        source_location: SourceLocation,
    ) {
        self.uses.push(CheckedApplicationTypeUse::ObjectReference(
            CheckedObjectReferenceUse {
                target,
                kind,
                location: source_location,
            },
        ));
    }

    fn next_expression_kind(&mut self) -> CheckedTypeUseKind {
        let ordinal = self.next_expression_ordinal;
        self.next_expression_ordinal += 1;
        CheckedTypeUseKind::Expression {
            owner: self.owner,
            ordinal,
        }
    }

    const fn result_kind(&self, ordinal: u32) -> CheckedTypeUseKind {
        CheckedTypeUseKind::Result {
            owner: self.owner,
            ordinal,
        }
    }
}

fn record_resolved_type_use(
    uses: &mut Vec<CheckedApplicationTypeUse>,
    enabled: bool,
    kind: CheckedTypeUseKind,
    resolved: ResolvedApplicationType,
    source_location: SourceLocation,
) {
    if !enabled {
        return;
    }
    if let Some(type_id) = resolved.standard_value_type {
        uses.push(CheckedApplicationTypeUse::Value(CheckedValueTypeUse {
            type_id,
            kind,
            location: source_location,
        }));
    } else if let SemanticType::Named(target) = resolved.semantic_type {
        uses.push(CheckedApplicationTypeUse::Named {
            target,
            kind,
            location: source_location,
        });
    } else if let SemanticType::Reference { target } = resolved.semantic_type {
        uses.push(CheckedApplicationTypeUse::ObjectReference(
            CheckedObjectReferenceUse {
                target,
                kind,
                location: source_location,
            },
        ));
    }
}
