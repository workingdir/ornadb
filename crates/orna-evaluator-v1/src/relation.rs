use super::Value;

/// An evaluator-only relation plan. It is deliberately not part of the
/// canonical value boundary: relation plans may retain closures and are
/// materialized only by an explicit terminal or window observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RelationPlan {
    pub(super) source: String,
    pub(super) stages: Vec<RelationStage>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum RelationStage {
    Filter(Value),
    Map(Value),
    /// Lazily expands each upstream value through a transform returning a
    /// finite `List`, preserving source and inner order.
    FlatMap(Value),
    SortBy(Value),
    Distinct,
    /// Adjacent overlapping row pairs, preserving upstream order.
    ///
    /// Evaluation keeps the previous post-prefix row across source pages and
    /// stage applications; the stage index is absolute through `stage_offset`
    /// when buffered suffixes are evaluated.
    Pairs,
    Drop(usize),
    Take(usize),
}

impl RelationPlan {
    pub(super) fn new(source: String) -> Self {
        Self {
            source,
            stages: Vec::new(),
        }
    }

    pub(super) fn with_stage(mut self, stage: RelationStage) -> Self {
        self.stages.push(stage);
        self
    }
}
