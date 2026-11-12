//! Source spans and positions shared by every syntax tree node.

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourcePosition {
    pub line: u32,
    pub column: u32,
}
/// The file and 1-based Unicode-scalar coordinates of a [`SourceSpan`].
///
/// A span either has a complete location or none: the parts are produced
/// together by [`SourceSpan::located`], so they cannot describe different
/// files or a half-annotated range.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SpanLocation {
    pub file: String,
    pub start: SourcePosition,
    pub end: SourcePosition,
}
/// Half-open UTF-8 bytes. A location is optional for unsaved editor buffers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceSpan {
    pub start: usize,
    pub end: usize,
    pub location: Option<SpanLocation>,
}
impl SourceSpan {
    pub const fn new(start: usize, end: usize) -> Self {
        Self {
            start,
            end,
            location: None,
        }
    }
    pub fn join(self, other: Self) -> Self {
        let location = match (self.location, other.location) {
            (Some(left), Some(right)) if left.file == right.file => Some(SpanLocation {
                file: left.file,
                start: left.start,
                end: right.end,
            }),
            _ => None,
        };
        Self {
            start: self.start,
            end: other.end,
            location,
        }
    }
    pub fn located(mut self, file: impl Into<String>, source: &str) -> Self {
        self.location = Some(SpanLocation {
            file: file.into(),
            start: position_at(source, self.start),
            end: position_at(source, self.end),
        });
        self
    }
    /// The owning file, when this span was located in a real document.
    pub fn file(&self) -> Option<&str> {
        self.location
            .as_ref()
            .map(|location| location.file.as_str())
    }
    /// The start position, when this span was located in a real document.
    pub fn start_position(&self) -> Option<&SourcePosition> {
        self.location.as_ref().map(|location| &location.start)
    }
}
pub fn position_at(source: &str, at: usize) -> SourcePosition {
    // Offsets produced by the lexer and parser are always UTF-8 boundaries,
    // but `SourceSpan` is public and callers can construct an unchecked span.
    // Walk complete scalars instead of slicing at `at`; this keeps diagnostic
    // annotation total for malformed local spans, which admission will reject
    // later, while leaving valid boundary coordinates unchanged.
    let at = at.min(source.len());
    let mut line = 1;
    let mut column = 1;
    for (index, scalar) in source.char_indices() {
        let end = index + scalar.len_utf8();
        if end > at {
            break;
        }
        if scalar == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    SourcePosition { line, column }
}
