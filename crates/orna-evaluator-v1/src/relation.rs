use std::collections::VecDeque;

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
    /// Complete positional windows over the post-prefix source.
    ///
    /// The evaluator owns one [`RelationWindowState`] per stage so that a
    /// window can span source pages, buffered sort suffixes, and values
    /// produced by an upstream flat map.
    Window(usize, usize),
    Drop(usize),
    Take(usize),
}

/// Incremental state for a [`RelationStage::Window`] stage.
///
/// `pending` contains the values beginning at the next candidate window
/// position. Once a complete window is emitted, values before the next
/// candidate are removed for overlap, or incoming values are skipped for a
/// gap. No finalisation method exists intentionally: an incomplete trailing
/// window is never emitted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RelationWindowState {
    size: usize,
    step: usize,
    pending: VecDeque<Value>,
    skipped: usize,
}

impl RelationWindowState {
    /// Builds state only for the valid positive size and step domain.
    pub(super) fn try_new(size: usize, step: usize) -> Option<Self> {
        (size > 0 && step > 0).then(|| Self {
            size,
            step,
            pending: VecDeque::new(),
            skipped: 0,
        })
    }

    /// Feeds one ordered value and returns a complete window when available.
    ///
    /// The returned list owns its values, allowing the caller to pass it down
    /// the remaining relation stages while this state retains overlap values.
    pub(super) fn push(&mut self, value: Value) -> Option<Vec<Value>> {
        if self.skipped > 0 {
            self.skipped -= 1;
            return None;
        }

        self.pending.push_back(value);
        if self.pending.len() < self.size {
            return None;
        }

        let window = self.pending.iter().take(self.size).cloned().collect();
        if self.step < self.size {
            for _ in 0..self.step {
                self.pending.pop_front();
            }
        } else {
            self.pending.clear();
            self.skipped = self.step - self.size;
        }
        Some(window)
    }
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
