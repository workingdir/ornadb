//! Syntax-aware LSP folding and selection ranges.

use lsp_types::{FoldingRange, FoldingRangeKind, SelectionRange};
use orna_syntax::visit::Visit;
use orna_syntax::{Declaration, SyntaxSpan as SourceSpan, SyntaxTree, lex};

use crate::documents::PositionMapper;

pub(crate) fn selection_ranges(
    tree: &SyntaxTree,
    text: &str,
    positions: &[lsp_types::Position],
    mapper: &PositionMapper<'_>,
) -> Vec<SelectionRange> {
    let mut spans = vec![SourceSpan::new(0, text.len())];
    spans.extend(SpanCollector::collect(tree));
    if let Ok(tokens) = lex(text) {
        spans.extend(
            tokens
                .into_iter()
                .filter(|token| token.span.start < token.span.end)
                .map(|token| token.span),
        );
    }
    spans.extend(scan_comments(text).into_iter().map(|comment| comment.span));
    spans.retain(|span| span.start <= span.end && span.end <= text.len());
    spans.sort_by_key(|span| (span.end.saturating_sub(span.start), span.start, span.end));
    spans.dedup_by_key(|span| (span.start, span.end));
    let line_spans = source_line_spans(text);

    positions
        .iter()
        .copied()
        .map(|position| {
            let byte = mapper.byte_offset(position);
            let mut chain = Vec::new();
            for span in spans
                .iter()
                .filter(|span| contains_position(span, byte, text.len()))
            {
                if chain.last().is_none_or(|inner: &SourceSpan| {
                    span.start <= inner.start
                        && span.end >= inner.end
                        && (span.start < inner.start || span.end > inner.end)
                }) {
                    chain.push(span.clone());
                }
            }
            // Trivia-only positions still need a useful selectable range.
            // Add a line range only when the syntax tree has no node at the
            // cursor, so it cannot interrupt a semantic parent chain.
            if chain.len() <= 1 {
                let line_span = line_spans
                    .iter()
                    .find(|span| {
                        contains_position(span, byte, text.len())
                            || (span.start == span.end && span.start == byte)
                    })
                    .cloned();
                if let Some(line_span) = line_span
                    && !spans
                        .iter()
                        .any(|span| span.start == line_span.start && span.end == line_span.end)
                {
                    chain.insert(0, line_span);
                }
            }
            if chain.is_empty() {
                chain.push(SourceSpan::new(0, text.len()));
            }
            let mut selection = None;
            for span in chain.into_iter().rev() {
                selection = Some(SelectionRange {
                    range: mapper.range(&span),
                    parent: selection.map(Box::new),
                });
            }
            selection.expect("the source range is always available")
        })
        .collect()
}

pub(crate) fn folding_ranges(
    tree: &SyntaxTree,
    text: &str,
    mapper: &PositionMapper<'_>,
) -> Vec<FoldingRange> {
    let mut ranges = SpanCollector::collect(tree)
        .into_iter()
        .filter_map(|span| folding_range(&span, text.len(), mapper, None))
        .collect::<Vec<_>>();

    for span in import_group_spans(tree, text) {
        if let Some(range) =
            folding_range(&span, text.len(), mapper, Some(FoldingRangeKind::Imports))
        {
            ranges.push(range);
        }
    }
    for comment in comment_groups(text) {
        if let Some(range) = folding_range(
            &comment,
            text.len(),
            mapper,
            Some(FoldingRangeKind::Comment),
        ) {
            ranges.push(range);
        }
    }

    ranges.sort_by_key(|range| {
        (
            range.start_line,
            range.start_character.unwrap_or_default(),
            range.end_line,
            range.end_character.unwrap_or_default(),
            folding_kind_order(range.kind.as_ref()),
        )
    });
    ranges.dedup_by(|later, earlier| {
        later.start_line == earlier.start_line
            && later.start_character == earlier.start_character
            && later.end_line == earlier.end_line
            && later.end_character == earlier.end_character
    });
    ranges
}

fn contains_position(span: &SourceSpan, byte: usize, text_len: usize) -> bool {
    span.start <= byte && (byte < span.end || (byte == text_len && span.end == text_len))
}

fn source_line_spans(text: &str) -> Vec<SourceSpan> {
    let mut spans = Vec::new();
    let mut start = 0usize;
    for line in text.split_inclusive('\n') {
        let mut end = start + line.len();
        if line.ends_with('\n') {
            end -= 1;
        }
        if end > start && text.as_bytes().get(end - 1) == Some(&b'\r') {
            end -= 1;
        }
        spans.push(SourceSpan::new(start, end));
        start += line.len();
    }
    if text.is_empty() || !text.ends_with('\n') {
        spans.push(SourceSpan::new(start.min(text.len()), text.len()));
    } else {
        spans.push(SourceSpan::new(text.len(), text.len()));
    }
    spans
}

/// Collects every source range the tree carries, in one exhaustive walk.
struct SpanCollector {
    spans: Vec<SourceSpan>,
}

impl SpanCollector {
    fn collect(tree: &SyntaxTree) -> Vec<SourceSpan> {
        let mut collector = Self { spans: Vec::new() };
        collector.visit_tree(tree);
        collector.spans
    }
}

impl Visit for SpanCollector {
    fn visit_span(&mut self, span: &SourceSpan) {
        if span.start <= span.end {
            self.spans.push(span.clone());
        }
    }
}

fn import_group_spans(tree: &SyntaxTree, text: &str) -> Vec<SourceSpan> {
    let imports = tree
        .items
        .iter()
        .filter_map(|item| {
            matches!(item.declaration, Declaration::Use { .. }).then_some(&item.span)
        })
        .collect::<Vec<_>>();
    let mut groups = Vec::new();
    let mut first = None;
    let mut last = None;
    let mut count = 0usize;
    let flush = |first: &mut Option<usize>,
                 last: &mut Option<usize>,
                 count: &mut usize,
                 groups: &mut Vec<SourceSpan>| {
        if let (Some(start), Some(end)) = (*first, *last)
            && *count > 1
        {
            groups.push(SourceSpan::new(start, end));
        }
        *first = None;
        *last = None;
        *count = 0;
    };
    for span in imports {
        if let Some(previous_end) = last {
            let gap = text.get(previous_end..span.start).unwrap_or_default();
            if !gap.chars().all(char::is_whitespace) || gap.matches('\n').count() > 1 {
                flush(&mut first, &mut last, &mut count, &mut groups);
            }
        }
        first.get_or_insert(span.start);
        last = Some(span.end);
        count += 1;
    }
    flush(&mut first, &mut last, &mut count, &mut groups);
    groups
}

fn folding_range(
    span: &SourceSpan,
    text_len: usize,
    mapper: &PositionMapper<'_>,
    kind: Option<FoldingRangeKind>,
) -> Option<FoldingRange> {
    if span.start >= span.end || span.end > text_len {
        return None;
    }
    let start = mapper.position(span.start);
    let end = mapper.position(span.end);
    (start.line < end.line).then_some(FoldingRange {
        start_line: start.line,
        start_character: Some(start.character),
        end_line: end.line,
        end_character: Some(end.character),
        kind,
        collapsed_text: None,
    })
}

fn folding_kind_order(kind: Option<&FoldingRangeKind>) -> u8 {
    match kind {
        Some(FoldingRangeKind::Comment) => 0,
        Some(FoldingRangeKind::Imports) => 1,
        Some(FoldingRangeKind::Region) => 2,
        None => 3,
    }
}

#[derive(Clone)]
struct CommentSpan {
    span: SourceSpan,
    line_comment: bool,
}

fn comment_groups(text: &str) -> Vec<SourceSpan> {
    let comments = scan_comments(text);
    let mapper = PositionMapper::new(text);
    let mut groups = Vec::new();
    let mut line_group: Option<(usize, usize, usize, usize)> = None;
    for comment in comments {
        if comment.line_comment {
            let start_line = mapper.position(comment.span.start).line as usize;
            if let Some((start, previous_end, group_start_line, previous_line)) = line_group {
                if start_line == previous_line + 1 {
                    line_group = Some((start, comment.span.end, group_start_line, start_line));
                    continue;
                }
                if previous_line > group_start_line {
                    groups.push(SourceSpan::new(start, previous_end));
                }
                line_group = Some((comment.span.start, comment.span.end, start_line, start_line));
            } else {
                line_group = Some((comment.span.start, comment.span.end, start_line, start_line));
            }
        } else {
            if let Some((start, end, start_line, last_line)) = line_group.take()
                && last_line > start_line
            {
                groups.push(SourceSpan::new(start, end));
            }
            groups.push(comment.span);
        }
    }
    if let Some((start, end, start_line, last_line)) = line_group
        && last_line > start_line
    {
        groups.push(SourceSpan::new(start, end));
    }
    groups
}

fn scan_comments(text: &str) -> Vec<CommentSpan> {
    let bytes = text.as_bytes();
    let mut comments = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        if bytes[at] == b'"' {
            at += 1;
            while at < bytes.len() {
                match bytes[at] {
                    b'\\' => {
                        at += 1;
                        if at < bytes.len() {
                            at += text[at..].chars().next().map_or(1, char::len_utf8);
                        }
                    }
                    b'"' => {
                        at += 1;
                        break;
                    }
                    _ => at += text[at..].chars().next().map_or(1, char::len_utf8),
                }
            }
        } else if bytes.get(at..at + 2) == Some(b"//") {
            let start = at;
            at += 2;
            while at < bytes.len() && bytes[at] != b'\n' {
                at += text[at..].chars().next().map_or(1, char::len_utf8);
            }
            comments.push(CommentSpan {
                span: SourceSpan::new(start, at),
                line_comment: true,
            });
        } else if bytes.get(at..at + 2) == Some(b"/*") {
            let start = at;
            at += 2;
            let mut depth = 1usize;
            while at < bytes.len() && depth > 0 {
                if bytes.get(at..at + 2) == Some(b"/*") {
                    depth += 1;
                    at += 2;
                } else if bytes.get(at..at + 2) == Some(b"*/") {
                    depth -= 1;
                    at += 2;
                } else {
                    at += text[at..].chars().next().map_or(1, char::len_utf8);
                }
            }
            comments.push(CommentSpan {
                span: SourceSpan::new(start, at),
                line_comment: false,
            });
        } else {
            at += text[at..].chars().next().map_or(1, char::len_utf8);
        }
    }
    comments
}
