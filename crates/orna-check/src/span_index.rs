//! Containment lookups over the top-level item spans of one module.
//!
//! The parser recovers a module's items in source order and they do not
//! overlap, so the item that holds a range is found by binary search instead of
//! by testing every item for each diagnostic. A list that is not in that shape
//! is searched linearly, so the answer is the same for any input.

use std::ops::Range;

/// The byte ranges of one module's top-level items, indexed for containment
/// lookups.
pub(crate) struct ItemSpans {
    spans: Vec<Range<usize>>,
    /// Whether every span is well-formed and starts no earlier than the span
    /// before it ends, so both starts and ends never decrease.
    ordered: bool,
}

impl ItemSpans {
    /// Indexes `spans`, given in the order the parser recovered them.
    pub(crate) fn new(spans: Vec<Range<usize>>) -> Self {
        let ordered = spans.iter().all(|span| span.start <= span.end)
            && spans.windows(2).all(|pair| pair[0].end <= pair[1].start);
        Self { spans, ordered }
    }

    /// The index of the first span that contains `inner`, that is the first
    /// `i` with `spans[i].start <= inner.start && inner.end <= spans[i].end`.
    pub(crate) fn containing(&self, inner: &Range<usize>) -> Option<usize> {
        if !self.ordered {
            return self
                .spans
                .iter()
                .position(|span| span.start <= inner.start && inner.end <= span.end);
        }
        // Ends never decrease, so the spans that end at or after `inner.end`
        // form a suffix and its first span is the only candidate. Starts never
        // decrease either, so if that span starts after `inner.start`, every
        // later span does too.
        let index = self.spans.partition_point(|span| span.end < inner.end);
        self.spans
            .get(index)
            .is_some_and(|span| span.start <= inner.start)
            .then_some(index)
    }

    /// The span at `index`, as returned by [`Self::containing`].
    pub(crate) fn span(&self, index: usize) -> &Range<usize> {
        &self.spans[index]
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use super::ItemSpans;

    /// The scan that [`ItemSpans::containing`] must agree with.
    fn scanned(spans: &[Range<usize>], inner: &Range<usize>) -> Option<usize> {
        spans
            .iter()
            .position(|span| span.start <= inner.start && inner.end <= span.end)
    }

    /// Every range `start..end` with `start <= end <= max`.
    fn all_ranges(max: usize) -> impl Iterator<Item = Range<usize>> {
        (0..=max).flat_map(move |start| (start..=max).map(move |end| start..end))
    }

    #[test]
    fn ordered_spans_agree_with_the_scan_at_every_boundary() {
        // Touching, empty and gapped spans, in source order.
        let spans = vec![0..5, 5..5, 5..9, 12..12, 12..20, 25..30];
        let index = ItemSpans::new(spans.clone());
        assert!(index.ordered);
        for inner in all_ranges(32) {
            assert_eq!(
                index.containing(&inner),
                scanned(&spans, &inner),
                "{inner:?}"
            );
        }
    }

    #[test]
    fn overlapping_spans_fall_back_to_the_first_match() {
        let spans = vec![0..10, 2..4, 3..8];
        let index = ItemSpans::new(spans.clone());
        assert!(!index.ordered);
        for inner in all_ranges(10) {
            assert_eq!(
                index.containing(&inner),
                scanned(&spans, &inner),
                "{inner:?}"
            );
        }
    }

    #[test]
    fn inverted_span_falls_back_to_the_scan() {
        let spans = vec![Range { start: 4, end: 2 }, 6..9];
        let index = ItemSpans::new(spans.clone());
        assert!(!index.ordered);
        for inner in all_ranges(10) {
            assert_eq!(
                index.containing(&inner),
                scanned(&spans, &inner),
                "{inner:?}"
            );
        }
    }

    #[test]
    fn empty_list_contains_nothing() {
        let index = ItemSpans::new(Vec::new());
        assert_eq!(index.containing(&(0..0)), None);
        assert_eq!(index.containing(&(3..7)), None);
    }

    #[test]
    fn span_returns_the_indexed_range() {
        let index = ItemSpans::new(vec![0..4, 6..9]);
        let found = index.containing(&(7..8));
        assert_eq!(found.map(|i| index.span(i)), Some(&(6..9)));
    }
}
