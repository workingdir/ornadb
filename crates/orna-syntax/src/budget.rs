//! The nesting limit shared by the lexer and the parser.

/// Nesting depth against a fixed limit. It remembers whether the overflow has
/// been reported, so deeply nested input yields one diagnostic rather than one
/// per level.
#[derive(Clone, Copy)]
pub(crate) struct NestingBudget {
    limit: usize,
    depth: usize,
    reported: bool,
}

impl NestingBudget {
    pub(crate) const fn new(limit: usize) -> Self {
        Self {
            limit,
            depth: 0,
            reported: false,
        }
    }

    /// Enters one nesting level. `false` means the limit is reached and the
    /// construct must be skipped; every `true` is paired with [`Self::leave`].
    pub(crate) fn enter(&mut self) -> bool {
        if self.depth == self.limit {
            return false;
        }
        self.depth += 1;
        true
    }

    pub(crate) fn leave(&mut self) {
        self.depth -= 1;
    }

    /// `true` the first time it is asked since the last [`Self::reset`], so
    /// the caller reports the overflow exactly once.
    pub(crate) fn first_overflow(&mut self) -> bool {
        !std::mem::replace(&mut self.reported, true)
    }

    /// Starts a fresh budget, as for the next top-level item.
    pub(crate) fn reset(&mut self) {
        self.depth = 0;
        self.reported = false;
    }
}
