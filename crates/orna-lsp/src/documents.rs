//! Open-document storage and byte-to-position mapping.
//!
//! Orna source spans are byte ranges. LSP positions are line and UTF-16
//! code-unit offsets. This module owns both conversions and keeps the
//! document text that the analysis stages need.

use lsp_types::{Position, Range, Uri};
use orna_syntax::SourceSpan;

/// One open Orna source document.
#[derive(Clone, Debug)]
pub struct Document {
    /// The document URI as opened by the client.
    pub uri: Uri,
    /// The current source text.
    pub text: String,
    /// The client-reported document version.
    pub version: i32,
}

impl Document {
    /// Creates an open document.
    pub fn new(uri: Uri, text: String, version: i32) -> Self {
        Self { uri, text, version }
    }

    /// Returns the logical path used for compiler diagnostics.
    ///
    /// The full URI string is unique and always nonempty, which satisfies
    /// the source bundle contract.
    pub fn logical_path(&self) -> String {
        self.uri.as_str().to_owned()
    }
}

/// Converts between byte offsets and LSP positions for one source text.
///
/// The mapper borrows the text it was built from; rebuild it whenever the
/// text changes.
pub struct PositionMapper<'text> {
    text: &'text str,
    /// The byte offset of each line start.
    line_starts: Vec<usize>,
}

impl<'text> PositionMapper<'text> {
    /// Builds a mapper for one source text.
    pub fn new(text: &'text str) -> Self {
        let mut line_starts = vec![0usize];
        for (index, character) in text.char_indices() {
            if character == '\n' {
                line_starts.push(index + 1);
            }
        }
        Self { text, line_starts }
    }

    /// Returns the zero-based line containing this byte offset.
    fn line_of(&self, byte: usize) -> usize {
        self.line_starts
            .partition_point(|&start| start <= byte)
            .saturating_sub(1)
    }

    /// Returns the byte offset of the end of one line, excluding its
    /// terminator.
    fn line_end_byte(&self, line: usize) -> usize {
        match self.line_starts.get(line + 1) {
            Some(&next_start) => {
                let line_end = next_start.saturating_sub(1);
                if self.text.as_bytes().get(line_end.saturating_sub(1)) == Some(&b'\r') {
                    line_end.saturating_sub(1)
                } else {
                    line_end
                }
            }
            None => self.text.len(),
        }
    }

    /// Converts a byte offset to an LSP position.
    pub fn position(&self, byte: usize) -> Position {
        let byte = byte.min(self.text.len());
        let line = self.line_of(byte);
        let line_start = self.line_starts[line];
        let character = utf16_len(&self.text[line_start..byte]);
        Position {
            line: line as u32,
            character: character as u32,
        }
    }

    /// Converts an LSP position to a byte offset.
    pub fn byte_offset(&self, position: Position) -> usize {
        let line = (position.line as usize).min(self.line_starts.len() - 1);
        let line_start = self.line_starts[line];
        let line_end = self.line_end_byte(line);
        let mut character = position.character as usize;
        for (index, current) in self.text[line_start..line_end].char_indices() {
            if character == 0 {
                return line_start + index;
            }
            character = character.saturating_sub(current.len_utf16());
        }
        line_end
    }

    /// Converts a byte span to an LSP range.
    pub fn range(&self, span: &SourceSpan) -> Range {
        Range {
            start: self.position(span.start),
            end: self.position(span.end),
        }
    }

    /// Splits one byte range into per-line segments with UTF-16 lengths.
    ///
    /// LSP semantic tokens must lie on a single line. Multi-line tokens such
    /// as block comments are split into one segment per covered line.
    pub fn segments(&self, span: &SourceSpan) -> Vec<(Position, u32)> {
        let mut segments = Vec::new();
        let mut start = span.start.min(self.text.len());
        let end = span.end.min(self.text.len());
        while start < end {
            let line = self.line_of(start);
            let segment_end = self.line_end_byte(line).min(end);
            let position = self.position(start);
            let length = utf16_len(&self.text[start..segment_end]);
            segments.push((position, length as u32));
            start = if segment_end == end {
                segment_end
            } else {
                self.line_starts[line + 1]
            };
        }
        segments
    }
}

/// Returns the UTF-16 code-unit length of one string.
fn utf16_len(text: &str) -> usize {
    text.chars().map(|character| character.len_utf16()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_advance_across_multiline_tokens() {
        let text = "/* first\n😀 second\nthird */";
        let mapper = PositionMapper::new(text);
        let comment = orna_syntax::highlight(text)
            .into_iter()
            .find(|token| token.kind == orna_syntax::HighlightKind::Comment)
            .expect("multiline comment token");
        let span = SourceSpan {
            start: comment.range.start,
            end: comment.range.end,
        };

        assert_eq!(
            mapper.segments(&span),
            vec![
                (Position { line: 0, character: 0 }, 8),
                (Position { line: 1, character: 0 }, 9),
                (Position { line: 2, character: 0 }, 8),
            ]
        );
    }

    #[test]
    fn crlf_line_segments_exclude_carriage_return() {
        let text = "first\r\nsecond";
        let mapper = PositionMapper::new(text);
        let span = SourceSpan {
            start: 0,
            end: text.len(),
        };

        assert_eq!(
            mapper.segments(&span),
            vec![
                (Position { line: 0, character: 0 }, 5),
                (Position { line: 1, character: 0 }, 6),
            ]
        );
    }
}
