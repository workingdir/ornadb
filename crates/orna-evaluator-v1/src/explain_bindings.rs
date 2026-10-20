//! Host-catalogue lowering for `sys.explain(Query)`.
//!
//! A relation pipeline is lowered into the published `orna-sys-v1` query
//! description only where every operator has an exact representation. The
//! source must resolve through the host catalogue to a snapshot-pinned object;
//! no reference is ever synthesised. Stages without such a representation fail
//! closed with `sys.abi.provider_failed`.

use std::collections::BTreeMap;
use std::fmt::Debug;

use orna_foundation_v1::SafeText;
use orna_sys_v1::{ObjectRef, Plan, QueryPlanDescription, SnapshotRef, explain_query};

use crate::relation::{RelationPlan, RelationStage};
use crate::{DecimalValue, EvaluationError, Value, parse_decimal};

const UNAVAILABLE: &str = "sys.abi.unavailable";
const PROVIDER_FAILED: &str = "sys.abi.provider_failed";

/// Resolves relation source names to snapshot-pinned catalogue entries.
///
/// Implementations return the pin the host has already resolved. A source the
/// catalogue does not know yields `None`; no reference is invented.
pub trait ExplainCatalogue: Send + Sync + Debug {
    fn resolve_source(&self, source: &str) -> Option<(SnapshotRef, ObjectRef)>;
}

/// Explains one relation query.
///
/// Without a catalogue the host cannot pin the source, so the call reports
/// `sys.abi.unavailable`. Unrepresentable stages report
/// `sys.abi.provider_failed`.
pub(crate) fn explain(
    plan: &RelationPlan,
    catalogue: Option<&dyn ExplainCatalogue>,
) -> Result<Value, EvaluationError> {
    let catalogue = catalogue.ok_or_else(|| failure(UNAVAILABLE))?;
    if plan.source_union.is_some() || plan.source_values.is_some() {
        return Err(failure(PROVIDER_FAILED));
    }
    let (snapshot, source) = catalogue
        .resolve_source(&plan.source)
        .ok_or_else(|| failure(PROVIDER_FAILED))?;
    let (distinct, limit) = lower_stages(&plan.stages)?;
    let query = QueryPlanDescription {
        snapshot,
        source,
        source_statistics: None,
        joins: Vec::new(),
        predicate: None,
        projections: Vec::new(),
        distinct,
        ordering: Vec::new(),
        limit,
        mutations: Vec::new(),
        materialize_into: None,
    };
    let explained = explain_query(&query).map_err(|_| failure(PROVIDER_FAILED))?;
    plan_record(explained.plan())
}

/// Lowers `distinct` and `take` stages. Filter and map stages need a
/// snapshot-pinned `ExpressionRef` for their closure, which the evaluator does
/// not have, so they fail closed. A `take` followed by `distinct` is not the
/// same as `distinct` then `take`, so it also fails closed.
fn lower_stages(stages: &[RelationStage]) -> Result<(bool, Option<u64>), EvaluationError> {
    let mut distinct = false;
    let mut limit: Option<u64> = None;
    for stage in stages {
        match stage {
            RelationStage::Distinct => {
                if limit.is_some() {
                    return Err(failure(PROVIDER_FAILED));
                }
                distinct = true;
            }
            RelationStage::Take(count) => {
                let count = u64::try_from(*count).map_err(|_| failure(PROVIDER_FAILED))?;
                limit = Some(limit.map_or(count, |current| current.min(count)));
            }
            _ => return Err(failure(PROVIDER_FAILED)),
        }
    }
    Ok((distinct, limit))
}

/// Converts the explained `sys.Plan` row into a plain record value.
fn plan_record(plan: &Plan) -> Result<Value, EvaluationError> {
    // `explain_query` never populates warnings. Fail closed rather than
    // silently dropping any diagnostic a future planner attaches.
    if !plan.warnings().is_empty() {
        return Err(failure(PROVIDER_FAILED));
    }
    let estimated_cost = match plan.estimated_cost() {
        None => Value::Option(None),
        Some(text) => {
            let (coefficient, exponent10) = parse_decimal(text)?;
            Value::Option(Some(Box::new(Value::Decimal(DecimalValue::new(
                coefficient,
                exponent10,
            )?))))
        }
    };
    let mut fields = BTreeMap::new();
    fields.insert(
        "reference".to_owned(),
        Value::String(plan.reference().as_str().to_owned()),
    );
    fields.insert("id".to_owned(), Value::String(plan.id().to_owned()));
    fields.insert(
        "snapshot".to_owned(),
        Value::String(plan.snapshot().as_str().to_owned()),
    );
    fields.insert(
        "root".to_owned(),
        Value::String(plan.root().as_str().to_owned()),
    );
    fields.insert("estimated_cost".to_owned(), estimated_cost);
    fields.insert(
        "actual_available".to_owned(),
        Value::Bool(plan.actual_available()),
    );
    fields.insert("warnings".to_owned(), Value::List(Vec::new()));
    Ok(Value::Record(fields))
}

fn failure(code: &'static str) -> EvaluationError {
    EvaluationError::redacted(SafeText::new(code).expect("static safe code"))
}
