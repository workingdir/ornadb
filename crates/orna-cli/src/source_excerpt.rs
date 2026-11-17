//! The `--> file:line:col` locator and source excerpt a failed evaluation
//! prints under its error, in the layout rustc uses for its own diagnostics.

use std::fmt::Write as _;

/// The locator and excerpt for the bytes `start..end` of `source`, or `None`
/// when the range does not lie on character boundaries of that source.
///
/// The excerpt shows the line the span starts on, with carets under the part
/// of that line the span covers. A span running past its first line is marked
/// only up to the line's end.
pub(crate) fn render(file: &str, source: &str, start: usize, end: usize) -> Option<String> {
    if start > end
        || end > source.len()
        || !source.is_char_boundary(start)
        || !source.is_char_boundary(end)
    {
        return None;
    }
    let line_start = source[..start].rfind('\n').map_or(0, |index| index + 1);
    let line_end = source[start..]
        .find('\n')
        .map_or(source.len(), |index| start + index);
    let line_number = source[..line_start].matches('\n').count() + 1;
    let column = source[line_start..start].chars().count() + 1;
    let line = &source[line_start..line_end];
    let marked_end = end.min(line_end);
    let caret_count = source[start..marked_end].chars().count().max(1);

    let gutter = line_number.to_string();
    let pad = " ".repeat(gutter.len());
    let lead = " ".repeat(column - 1);
    let carets = "^".repeat(caret_count);
    let mut rendered = String::new();
    // Writing to a `String` cannot fail.
    let _ = write!(
        rendered,
        "{pad}--> {file}:{line_number}:{column}\n{pad} |\n{gutter} | {line}\n{pad} | {lead}{carets}"
    );
    Some(rendered)
}

#[cfg(test)]
mod tests {
    use super::render;

    #[test]
    fn marks_the_span_under_its_line() {
        let source = "fn main() {\n    1 / 0\n}\n";
        let start = source.find("1 / 0").expect("fixture contains the span");
        let rendered = render("main.orna", source, start, start + 5).expect("in range");
        assert_eq!(
            rendered,
            " --> main.orna:2:5\n  |\n2 |     1 / 0\n  |     ^^^^^"
        );
    }

    #[test]
    fn rejects_ranges_outside_the_source() {
        assert_eq!(render("main.orna", "ab", 1, 9), None);
        assert_eq!(render("main.orna", "ab", 2, 1), None);
    }
}
