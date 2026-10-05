//! Lossless whitespace formatting for open Orna documents.
//!
//! The syntax 1.0 frontend intentionally has no source rewriter. Formatting
//! therefore uses its lexer only to find delimiter nesting, then edits line
//! prefixes and optional trailing whitespace while preserving code bytes and
//! line endings. Multiline strings and block comments remain untouched.

use lsp_types::{FormattingOptions, Position, Range, TextEdit};
use orna_syntax_v1::{Token, TokenKind, lex};

use crate::documents::PositionMapper;

#[derive(Clone, Copy, Debug)]
struct SourceLine {
    start: usize,
    content_end: usize,
    end: usize,
}

/// Formats a document or a line-bounded LSP range.
///
/// Invalid/incomplete lexing yields no edits. This keeps a formatter request
/// safe while the user is midway through typing a string or another token.
pub(crate) fn format(
    source: &str,
    requested_range: Option<Range>,
    options: &FormattingOptions,
) -> Vec<TextEdit> {
    let Ok(tokens) = lex(source) else {
        return Vec::new();
    };
    let lines = source_lines(source);
    let Some((first_line, last_line)) = selected_lines(&lines, requested_range) else {
        return Vec::new();
    };
    let protected = multiline_literal_lines(source, &lines, &tokens);
    let indentation = indentation_for(options);
    let trim_trailing = options.trim_trailing_whitespace.unwrap_or(true);
    let mut depth = 0usize;
    let mut formatted = String::with_capacity(source.len());

    for (line_index, line) in lines.iter().copied().enumerate() {
        let original = &source[line.start..line.end];
        if line_index < first_line || line_index > last_line || protected[line_index] {
            formatted.push_str(original);
            depth = update_depth(depth, &tokens, line.start, line.content_end);
            continue;
        }

        let content = &source[line.start..line.content_end];
        let first_non_whitespace = content
            .find(|character: char| !matches!(character, ' ' | '\t'))
            .unwrap_or(content.len());
        if first_non_whitespace == content.len() {
            if trim_trailing {
                formatted.push_str(&source[line.content_end..line.end]);
            } else {
                formatted.push_str(original);
            }
            depth = update_depth(depth, &tokens, line.start, line.content_end);
            continue;
        }

        let line_tokens = tokens_on_line(&tokens, line.start, line.content_end);
        let leading_closers = line_tokens
            .iter()
            .take_while(|token| is_closing_delimiter(token))
            .count();
        let visible_depth = depth.saturating_sub(leading_closers);
        let formatted_content_end = if trim_trailing {
            content
                .rfind(|character: char| !matches!(character, ' ' | '\t'))
                .map_or(first_non_whitespace, |index| index + 1)
        } else {
            content.len()
        };
        formatted.push_str(&indentation.repeat(visible_depth));
        formatted.push_str(&content[first_non_whitespace..formatted_content_end]);
        formatted.push_str(&source[line.content_end..line.end]);
        depth = update_depth(depth, &tokens, line.start, line.content_end);
    }

    if requested_range.is_none() {
        apply_final_newline_options(&mut formatted, source, options);
    }
    if formatted == source {
        return Vec::new();
    }

    let mapper = PositionMapper::new(source);
    if requested_range.is_none() {
        return vec![TextEdit {
            range: Range {
                start: Position::new(0, 0),
                end: mapper.position(source.len()),
            },
            new_text: formatted,
        }];
    }

    let start = lines[first_line].start;
    let end = lines[last_line].end;
    // The selected lines can change byte length, so find their replacement by
    // anchoring on the unchanged prefix and suffix rather than reusing the old
    // byte width.
    selected_replacement(source, &formatted, start, end, &mapper)
}

fn selected_replacement(
    source: &str,
    formatted: &str,
    start: usize,
    end: usize,
    mapper: &PositionMapper<'_>,
) -> Vec<TextEdit> {
    let prefix_len = start.min(formatted.len());
    let suffix_len = source.len().saturating_sub(end);
    if formatted.len() < prefix_len + suffix_len
        || formatted.get(..prefix_len) != source.get(..start)
        || formatted.get(formatted.len() - suffix_len..) != source.get(end..)
    {
        return Vec::new();
    }
    vec![TextEdit {
        range: Range {
            start: mapper.position(start),
            end: mapper.position(end),
        },
        new_text: formatted[prefix_len..formatted.len() - suffix_len].to_owned(),
    }]
}

fn source_lines(source: &str) -> Vec<SourceLine> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, byte) in source.bytes().enumerate() {
        if byte == b'\n' {
            let end = index + 1;
            let content_end = if index > start && source.as_bytes()[index - 1] == b'\r' {
                index - 1
            } else {
                index
            };
            lines.push(SourceLine {
                start,
                content_end,
                end,
            });
            start = end;
        }
    }
    if start < source.len() || lines.is_empty() || source.ends_with('\n') {
        lines.push(SourceLine {
            start,
            content_end: source.len(),
            end: source.len(),
        });
    }
    lines
}

fn selected_lines(lines: &[SourceLine], range: Option<Range>) -> Option<(usize, usize)> {
    let Some(range) = range else {
        return Some((0, lines.len() - 1));
    };
    if range.start > range.end {
        return None;
    }
    let first = (range.start.line as usize).min(lines.len() - 1);
    let mut last = (range.end.line as usize).min(lines.len() - 1);
    if range.end.character == 0 && last > first {
        last -= 1;
    }
    (last >= first).then_some((first, last))
}

fn indentation_for(options: &FormattingOptions) -> String {
    if options.insert_spaces {
        " ".repeat(options.tab_size.clamp(1, 16) as usize)
    } else {
        "\t".to_owned()
    }
}

fn tokens_on_line(tokens: &[Token], start: usize, end: usize) -> Vec<&Token> {
    tokens
        .iter()
        .filter(|token| token.span.start >= start && token.span.start < end)
        .filter(|token| !matches!(token.kind, TokenKind::Eof))
        .collect()
}

fn is_closing_delimiter(token: &&Token) -> bool {
    matches!(token.kind, TokenKind::Punct(")" | "]" | "}"))
}

fn update_depth(mut depth: usize, tokens: &[Token], start: usize, end: usize) -> usize {
    for token in tokens
        .iter()
        .filter(|token| token.span.start >= start && token.span.start < end)
    {
        match token.kind {
            TokenKind::Punct("(" | "[" | "{") => depth = depth.saturating_add(1),
            TokenKind::Punct(")" | "]" | "}") => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    depth
}

fn multiline_literal_lines(source: &str, lines: &[SourceLine], tokens: &[Token]) -> Vec<bool> {
    let mut protected = vec![false; lines.len()];
    for token in tokens {
        let is_string = matches!(
            token.kind,
            TokenKind::String
                | TokenKind::StringStart
                | TokenKind::StringText
                | TokenKind::StringEnd
        );
        if !is_string || !source[token.span.start..token.span.end].contains('\n') {
            continue;
        }
        protect_span(lines, &mut protected, token.span.start, token.span.end);
    }

    // Comments are trivia to the parser but are included in the public editor
    // token stream. Only multiline comments need protection; ordinary comment
    // indentation and trailing whitespace remain formatable.
    for token in orna_syntax_v1::editor::highlight(source) {
        if token.class == orna_syntax_v1::editor::TokenClass::Comment
            && source[token.range.clone()].contains('\n')
        {
            protect_span(lines, &mut protected, token.range.start, token.range.end);
        }
    }
    protected
}

fn protect_span(lines: &[SourceLine], protected: &mut [bool], start: usize, end: usize) {
    for (index, line) in lines.iter().enumerate() {
        if start < line.end && end > line.start {
            protected[index] = true;
        }
    }
}

fn apply_final_newline_options(
    formatted: &mut String,
    original: &str,
    options: &FormattingOptions,
) {
    let newline = if original.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    if options.trim_final_newlines == Some(true) {
        loop {
            if formatted.ends_with("\r\n\r\n") {
                formatted.truncate(formatted.len() - 2);
            } else if formatted.ends_with("\n\n") {
                formatted.pop();
            } else {
                break;
            }
        }
    }
    if options.insert_final_newline == Some(true) && !formatted.ends_with('\n') {
        formatted.push_str(newline);
    }
}
