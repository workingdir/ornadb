//! Shared source diagnostic rendering for source commands.

use std::io::{self, Write};

use orna_compiler::{CompilerDiagnostic, DiagnosticSeverity, ParseReport, SourceLocation};

/// Renders compiler diagnostics in the stable source-command wire format.
pub(crate) fn render_diagnostics(diagnostics: &[CompilerDiagnostic]) -> Vec<u8> {
    let mut output = Vec::new();
    write_diagnostics(&mut output, diagnostics).expect("writing diagnostics to Vec cannot fail");
    output
}

/// Renders compiler diagnostics as a Rust-style human-readable report.
pub(crate) fn render_human_diagnostics(
    parse_report: &ParseReport,
    diagnostics: &[CompilerDiagnostic],
    colour: bool,
) -> Vec<u8> {
    let mut output = Vec::new();
    write_human_diagnostics(&mut output, parse_report, diagnostics, colour)
        .expect("writing diagnostics to Vec cannot fail");
    output
}

/// Writes compiler diagnostics in source order, preserving the machine format.
pub(crate) fn write_diagnostics(
    output: &mut impl Write,
    diagnostics: &[CompilerDiagnostic],
) -> io::Result<()> {
    for diagnostic in diagnostics {
        let location = diagnostic.location();
        let span = location.span();
        write!(
            output,
            "{}:{}..{}: {}: ",
            location.logical_path(),
            span.start(),
            span.end(),
            diagnostic.code().as_str(),
        )?;
        write_escaped_message(output, diagnostic.message())?;
        output.write_all(b"\n")?;
    }
    Ok(())
}
fn display_column(text: &str) -> usize {
    text.chars()
        .map(|character| match character {
            '\t' => 4,
            '\r' => 0,
            character if source_character_is_escaped(character) => {
                format!("\\u{{{:04X}}}", character as u32).chars().count()
            }
            _ => 1,
        })
        .sum()
}

fn write_human_diagnostics(
    output: &mut impl Write,
    parse_report: &ParseReport,
    diagnostics: &[CompilerDiagnostic],
    colour: bool,
) -> io::Result<()> {
    for (index, diagnostic) in diagnostics.iter().enumerate() {
        if index != 0 {
            output.write_all(b"\n")?;
        }
        write_human_diagnostic(output, parse_report, diagnostic, colour)?;
    }
    if !diagnostics.is_empty() {
        output.write_all(b"\n")?;
        write_summary(output, diagnostics, colour)?;
    }
    Ok(())
}

fn write_human_diagnostic(
    output: &mut impl Write,
    parse_report: &ParseReport,
    diagnostic: &CompilerDiagnostic,
    colour: bool,
) -> io::Result<()> {
    write_level(output, diagnostic.severity(), colour)?;
    write!(output, "[{}]: ", diagnostic.code().as_str(),)?;
    write_human_text(output, diagnostic.message())?;
    output.write_all(b"\n")?;

    let gutter_width = write_source_annotation(
        output,
        parse_report,
        diagnostic.location(),
        diagnostic.primary_label(),
        AnnotationKind::Primary(diagnostic.severity()),
        colour,
    )?;
    for related in diagnostic.related() {
        write_source_annotation(
            output,
            parse_report,
            related.location(),
            related.message(),
            AnnotationKind::Related,
            colour,
        )?;
    }

    let help = help_for(diagnostic);
    if help.is_some() || !diagnostic.notes().is_empty() {
        writeln!(output, "{:>gutter_width$} |", "")?;
    }
    if let Some(help) = help {
        write_metadata_name(output, "help", "\x1b[1;32m", colour, gutter_width)?;
        output.write_all(b": ")?;
        write_human_text(output, help)?;
        output.write_all(b"\n")?;
    }
    for note in diagnostic.notes() {
        write_metadata_name(output, "note", "\x1b[1;36m", colour, gutter_width)?;
        output.write_all(b": ")?;
        write_human_text(output, note)?;
        output.write_all(b"\n")?;
    }
    Ok(())
}

fn write_source_annotation(
    output: &mut impl Write,
    parse_report: &ParseReport,
    location: &SourceLocation,
    label: &str,
    kind: AnnotationKind,
    colour: bool,
) -> io::Result<usize> {
    let arrow = match kind {
        AnnotationKind::Primary(_) => "-->",
        AnnotationKind::Related => ":::",
    };
    let Some(unit) = parse_report
        .units()
        .iter()
        .find(|unit| unit.logical_path() == location.logical_path())
    else {
        if colour {
            write!(output, "  \x1b[36m{arrow}\x1b[0m ")?;
        } else {
            write!(output, "  {arrow} ")?;
        }
        write!(
            output,
            "{}:{}..{}: ",
            location.logical_path(),
            location.span().start(),
            location.span().end(),
        )?;
        write_human_text(output, label)?;
        output.write_all(b"\n")?;
        return Ok(2);
    };

    let source = unit.source_text();
    let lines = source_lines(source);
    let start = char_boundary_at_or_before(source, location.span().start().min(source.len()));
    let end = char_boundary_at_or_after(source, location.span().end().min(source.len()).max(start));
    let start_line = source_line_index(&lines, start);
    let end_location = if end > start { end - 1 } else { start };
    let end_line = source_line_index(&lines, end_location);
    let column = display_column(&source[lines[start_line].start..start]) + 1;
    let gutter_width = decimal_width(end_line + 1).max(2);

    if colour {
        write!(output, "  \x1b[36m{arrow}\x1b[0m ")?;
    } else {
        write!(output, "  {arrow} ")?;
    }
    writeln!(
        output,
        "{}:{}:{}",
        location.logical_path(),
        start_line + 1,
        column
    )?;
    writeln!(output, "{:>gutter_width$} |", "")?;

    for selected in selected_source_lines(start_line, end_line) {
        let Some(line_index) = selected else {
            writeln!(output, "{:>gutter_width$} | ...", "")?;
            continue;
        };
        let line = lines[line_index];
        let raw_line = &source[line.start..line.end];
        let marker_start_byte = if line_index == start_line {
            start.saturating_sub(line.start).min(raw_line.len())
        } else {
            0
        };
        let marker_end_byte = if line_index == end_line {
            end.saturating_sub(line.start).min(raw_line.len())
        } else {
            raw_line.len()
        }
        .max(marker_start_byte);
        let marker_start = display_column(&raw_line[..marker_start_byte]);
        let marker_width = display_column(&raw_line[marker_start_byte..marker_end_byte]).max(1);
        let (rendered_line, marker_start, marker_width) =
            clip_source_line(raw_line, marker_start, marker_width);

        writeln!(
            output,
            "{:>gutter_width$} | {rendered_line}",
            line_index + 1
        )?;
        write!(
            output,
            "{:>gutter_width$} | {}",
            "",
            " ".repeat(marker_start)
        )?;
        let marker = kind.marker().to_string().repeat(marker_width);
        if colour {
            write!(output, "{}{marker}\x1b[0m", kind.colour())?;
        } else {
            write!(output, "{marker}")?;
        }
        if line_index == end_line {
            if colour {
                write!(output, " {}", kind.colour())?;
                write_human_text(output, label)?;
                write!(output, "\x1b[0m")?;
            } else {
                write!(output, " ")?;
                write_human_text(output, label)?;
            }
            if end == source.len() && start == source.len() {
                write!(output, " EOF")?;
            }
        }
        output.write_all(b"\n")?;
    }
    Ok(gutter_width)
}

fn write_level(
    output: &mut impl Write,
    severity: DiagnosticSeverity,
    colour: bool,
) -> io::Result<()> {
    if colour {
        write!(
            output,
            "\x1b[1;{}m{}\x1b[0m",
            severity_colour_code(severity),
            severity.as_str(),
        )
    } else {
        output.write_all(severity.as_str().as_bytes())
    }
}

fn write_metadata_name(
    output: &mut impl Write,
    name: &str,
    ansi: &str,
    colour: bool,
    gutter_width: usize,
) -> io::Result<()> {
    write!(output, "{:>gutter_width$} = ", "")?;
    if colour {
        write!(output, "{ansi}{name}\x1b[0m")
    } else {
        output.write_all(name.as_bytes())
    }
}

fn write_summary(
    output: &mut impl Write,
    diagnostics: &[CompilerDiagnostic],
    colour: bool,
) -> io::Result<()> {
    let error_count = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity() == DiagnosticSeverity::Error)
        .count();
    let warning_count = diagnostics.len() - error_count;
    if error_count != 0 {
        write_level(output, DiagnosticSeverity::Error, colour)?;
        write!(
            output,
            ": aborting due to {error_count} previous error{}",
            plural_suffix(error_count)
        )?;
        if warning_count != 0 {
            write!(
                output,
                "; {warning_count} warning{} emitted",
                plural_suffix(warning_count)
            )?;
        }
        output.write_all(b"\n")
    } else {
        write_level(output, DiagnosticSeverity::Warning, colour)?;
        writeln!(
            output,
            ": {warning_count} warning{} emitted",
            plural_suffix(warning_count)
        )
    }
}

fn plural_suffix(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn severity_colour_code(severity: DiagnosticSeverity) -> u8 {
    match severity {
        DiagnosticSeverity::Error => 31,
        DiagnosticSeverity::Warning => 33,
    }
}

#[derive(Clone, Copy)]
enum AnnotationKind {
    Primary(DiagnosticSeverity),
    Related,
}

impl AnnotationKind {
    fn marker(self) -> char {
        match self {
            Self::Primary(_) => '^',
            Self::Related => '-',
        }
    }

    fn colour(self) -> &'static str {
        match self {
            Self::Primary(DiagnosticSeverity::Error) => "\x1b[31m",
            Self::Primary(DiagnosticSeverity::Warning) => "\x1b[33m",
            Self::Related => "\x1b[36m",
        }
    }
}

#[derive(Clone, Copy)]
struct SourceLine {
    start: usize,
    end: usize,
}

fn source_lines(source: &str) -> Vec<SourceLine> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, byte) in source.bytes().enumerate() {
        if byte == b'\n' {
            lines.push(SourceLine { start, end: index });
            start = index + 1;
        }
    }
    lines.push(SourceLine {
        start,
        end: source.len(),
    });
    lines
}

fn source_line_index(lines: &[SourceLine], offset: usize) -> usize {
    lines
        .iter()
        .position(|line| offset <= line.end)
        .unwrap_or_else(|| lines.len().saturating_sub(1))
}

fn selected_source_lines(start: usize, end: usize) -> Vec<Option<usize>> {
    if end.saturating_sub(start) < 4 {
        return (start..=end).map(Some).collect();
    }
    vec![Some(start), Some(start + 1), None, Some(end)]
}

fn decimal_width(number: usize) -> usize {
    number.to_string().len()
}

fn clip_source_line(
    raw_line: &str,
    marker_start: usize,
    marker_width: usize,
) -> (String, usize, usize) {
    const MAX_COLUMNS: usize = 120;
    const LEADING_CONTEXT: usize = 36;

    let rendered = render_source_line(raw_line);
    let characters = rendered.chars().collect::<Vec<_>>();
    if characters.len() <= MAX_COLUMNS {
        return (rendered, marker_start, marker_width);
    }

    let marker_end = marker_start.saturating_add(marker_width);
    let mut window_start = marker_start.saturating_sub(LEADING_CONTEXT);
    let mut window_end = (window_start + MAX_COLUMNS).min(characters.len());
    if marker_end > window_end {
        window_start = marker_end.saturating_sub(MAX_COLUMNS);
        window_end = (window_start + MAX_COLUMNS).min(characters.len());
    }
    let leading_ellipsis = window_start != 0;
    let trailing_ellipsis = window_end != characters.len();
    let mut clipped = String::new();
    if leading_ellipsis {
        clipped.push_str("...");
    }
    clipped.extend(characters[window_start..window_end].iter());
    if trailing_ellipsis {
        clipped.push_str("...");
    }

    let visible_start = marker_start.max(window_start);
    let visible_end = marker_end.min(window_end).max(visible_start + 1);
    (
        clipped,
        visible_start - window_start + usize::from(leading_ellipsis) * 3,
        visible_end - visible_start,
    )
}

fn render_source_line(line: &str) -> String {
    let mut rendered = String::new();
    for character in line.chars() {
        match character {
            '\t' => rendered.push_str("    "),
            '\r' => {}
            character if source_character_is_escaped(character) => {
                rendered.push_str(&format!("\\u{{{:04X}}}", character as u32));
            }
            character => rendered.push(character),
        }
    }
    rendered
}

fn source_character_is_escaped(character: char) -> bool {
    character.is_control()
        || matches!(character, '\u{2028}' | '\u{2029}')
        || is_format_control(character)
}

fn is_format_control(character: char) -> bool {
    matches!(
        character,
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    )
}

fn help_for(diagnostic: &CompilerDiagnostic) -> Option<&str> {
    if diagnostic.code().as_str() == "ORNA0001" && diagnostic.message().contains("schema name") {
        return Some("write a schema name before the semicolon");
    }
    diagnostic.help()
}

fn char_boundary_at_or_before(source: &str, offset: usize) -> usize {
    let mut boundary = offset.min(source.len());
    while boundary > 0 && !source.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

fn char_boundary_at_or_after(source: &str, offset: usize) -> usize {
    let mut boundary = offset.min(source.len());
    while boundary < source.len() && !source.is_char_boundary(boundary) {
        boundary += 1;
    }
    boundary
}
fn write_escaped_message(output: &mut impl Write, message: &str) -> io::Result<()> {
    for character in message.chars() {
        match character {
            '\\' => output.write_all(b"\\\\")?,
            '\n' => output.write_all(b"\\n")?,
            '\r' => output.write_all(b"\\r")?,
            '\t' => output.write_all(b"\\t")?,
            '\u{2028}' | '\u{2029}' => {
                write!(output, "\\u{{{:04X}}}", character as u32)?;
            }
            character if character.is_control() => {
                write!(output, "\\u{{{:04X}}}", character as u32)?;
            }
            character => {
                let mut encoded = [0; 4];
                output.write_all(character.encode_utf8(&mut encoded).as_bytes())?;
            }
        }
    }
    Ok(())
}

fn write_human_text(output: &mut impl Write, text: &str) -> io::Result<()> {
    for character in text.chars() {
        match character {
            '\n' => output.write_all(b"\\n")?,
            '\r' => output.write_all(b"\\r")?,
            '\t' => output.write_all(b"\\t")?,
            character if source_character_is_escaped(character) => {
                write!(output, "\\u{{{:04X}}}", character as u32)?;
            }
            character => {
                let mut encoded = [0; 4];
                output.write_all(character.encode_utf8(&mut encoded).as_bytes())?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_core::source::{SourceBundle, SourceUnit};

    fn report_for(source_text: &str) -> orna_compiler::StandardApplicationCheckReport {
        report_for_path("main.orna", source_text)
    }

    fn report_for_path(
        logical_path: &str,
        source_text: &str,
    ) -> orna_compiler::StandardApplicationCheckReport {
        let source =
            SourceBundle::new([SourceUnit::new(logical_path, source_text)]).expect("source bundle");
        let standard = orna_compiler::check_standard_library_source(
            &orna_standard::retained_standard_library_v11_snapshot()
                .and_then(orna_standard::verify_standard_library_v11_snapshot)
                .expect("standard snapshot"),
        )
        .expect("standard source");
        orna_compiler::check_new_application(&source, &standard).expect("source check")
    }

    fn broken_report() -> orna_compiler::StandardApplicationCheckReport {
        report_for("CREATE SCHEMA ;")
    }

    #[test]
    fn renders_source_context_with_line_column_caret_and_help() {
        let report = broken_report();
        let rendered = String::from_utf8(render_human_diagnostics(
            report.parse_report(),
            report.diagnostics(),
            false,
        ))
        .expect("diagnostics are UTF-8");
        assert!(rendered.contains("error[ORNA0001]: expected a schema name after CREATE SCHEMA"));
        assert!(rendered.contains("  --> main.orna:1:15"));
        assert!(rendered.contains("1 | CREATE SCHEMA ;"));
        assert!(rendered.contains("^"));
        assert!(rendered.contains("= help: write a schema name before the semicolon"));
    }
    #[test]
    fn renders_zero_width_eof_span_at_source_length() {
        let source = "CREATE SCHEMA ";
        let report = report_for(source);
        let diagnostic = report
            .diagnostics()
            .first()
            .expect("source check should report missing schema name");
        assert_eq!(diagnostic.location().span().start(), source.len());
        assert_eq!(diagnostic.location().span().end(), source.len());

        let rendered = String::from_utf8(render_human_diagnostics(
            report.parse_report(),
            std::slice::from_ref(diagnostic),
            false,
        ))
        .expect("diagnostics are UTF-8");
        assert!(rendered.contains("  --> main.orna:1:15"));
        assert!(rendered.contains(" 1 | CREATE SCHEMA "));
        let marker = rendered
            .lines()
            .find(|line| line.contains("^"))
            .expect("EOF caret line");
        assert_eq!(
            marker,
            format!("   | {}^ unexpected syntax EOF", " ".repeat(source.len()))
        );
    }

    #[test]
    fn colour_output_contains_ansi_only_when_requested() {
        let report = broken_report();
        let rendered = render_human_diagnostics(report.parse_report(), report.diagnostics(), true);
        assert!(String::from_utf8_lossy(&rendered).contains("\x1b[1;31m"));
    }

    #[test]
    fn machine_diagnostics_keep_the_existing_contract() {
        let report = broken_report();
        assert_eq!(
            render_diagnostics(report.diagnostics()),
            b"main.orna:14..15: ORNA0001: expected a schema name after CREATE SCHEMA\n"
        );
    }

    #[test]
    fn expands_tabs_without_panicking() {
        assert_eq!(display_column("\tX"), 5);
    }
    #[test]
    fn aligns_caret_after_escaped_control_character() {
        let report = report_for("bad\u{0007}x");
        let diagnostic = report
            .diagnostics()
            .first()
            .expect("source check should report invalid syntax");
        let rendered = String::from_utf8(render_human_diagnostics(
            report.parse_report(),
            std::slice::from_ref(diagnostic),
            false,
        ))
        .expect("diagnostics are UTF-8");
        assert!(rendered.contains("1 | bad\\u{0007}x"));
        let marker = rendered
            .lines()
            .find(|line| line.contains("^"))
            .expect("caret line");
        assert_eq!(marker, "   | ^^^ unexpected syntax");
    }

    #[test]
    fn renders_related_duplicate_location_and_error_summary() {
        let report = report_for("CREATE SCHEMA app;\nCREATE SCHEMA app;");
        let rendered = String::from_utf8(render_human_diagnostics(
            report.parse_report(),
            report.diagnostics(),
            false,
        ))
        .expect("diagnostics are UTF-8");

        assert!(rendered.contains("error[ORNA0103]: duplicate schema definition app"));
        assert!(rendered.contains("^^^ redefined here"));
        assert!(rendered.contains("  ::: main.orna:1:15"));
        assert!(rendered.contains("--- first defined here"));
        assert!(rendered.contains("= help: rename one of the definitions or remove the duplicate"));
        assert!(rendered.ends_with("error: aborting due to 1 previous error\n"));
    }
    #[test]
    fn escapes_hostile_quoted_name_in_human_header_and_excerpt() {
        let name = "quoted\nname\r\t\u{2028}\u{2029}\u{202E}";
        let source = format!("CREATE SCHEMA \"{name}\";\nCREATE SCHEMA \"{name}\";");
        let report = report_for(&source);
        let rendered = String::from_utf8(render_human_diagnostics(
            report.parse_report(),
            report.diagnostics(),
            false,
        ))
        .expect("diagnostics are UTF-8");

        assert!(rendered.contains(
            "error[ORNA0103]: duplicate schema definition quoted\\nname\\r\\t\\u{2028}\\u{2029}\\u{202E}"
        ));
        assert!(rendered.contains("name    \\u{2028}\\u{2029}\\u{202E}\";"));
        assert!(!rendered.contains('\r'));
        assert!(!rendered.contains('\u{2028}'));
        assert!(!rendered.contains('\u{2029}'));
        assert!(!rendered.contains('\u{202E}'));
    }

    #[test]
    fn retains_pathless_fallback_annotation_format() {
        let source_report = report_for("CREATE SCHEMA ;");
        let foreign_report = report_for_path("other.orna", "CREATE SCHEMA ;");
        let rendered = String::from_utf8(render_human_diagnostics(
            source_report.parse_report(),
            foreign_report.diagnostics(),
            false,
        ))
        .expect("diagnostics are UTF-8");

        assert!(rendered.contains("  --> other.orna:14..15: unexpected syntax"));
        assert!(!rendered.contains("1 | CREATE SCHEMA ;"));
    }

    #[test]
    fn renders_nonblocking_warning_with_note_and_summary() {
        let report = report_for(
            "CREATE SCHEMA app;\n\
             CREATE CLIENT FUNCTION app.unreachable()\n\
             RETURNS BOOLEAN\n\
             IS\n\
             BEGIN\n\
                 RETURN TRUE;\n\
                 LET ignored := FALSE;\n\
             END;",
        );
        assert!(!report.has_errors());
        let rendered = String::from_utf8(render_human_diagnostics(
            report.parse_report(),
            report.diagnostics(),
            false,
        ))
        .expect("diagnostics are UTF-8");

        assert!(rendered.contains("warning[ORNA0401]: unreachable statement"));
        assert!(rendered.contains("^^^^"));
        assert!(rendered.contains("unreachable code"));
        assert!(rendered.contains("  ::: main.orna:6:1"));
        assert!(rendered.contains("this statement returns from the function"));
        assert!(
            rendered
                .contains("= note: unreachable statements are still checked but can never execute")
        );
        assert!(rendered.ends_with("warning: 1 warning emitted\n"));
    }
    #[test]
    fn escapes_human_metadata_without_physical_control_effects() {
        let mut output = Vec::new();
        write_human_text(
            &mut output,
            "a\n\r\t\u{0007}\u{2028}\u{2029}\u{202E}\u{200B}é",
        )
        .expect("Vec accepts every write");
        assert_eq!(
            output,
            "a\\n\\r\\t\\u{0007}\\u{2028}\\u{2029}\\u{202E}\\u{200B}é".as_bytes()
        );
    }
}

#[cfg(test)]
mod source_context_tests {
    use super::*;

    #[test]
    fn display_column_counts_utf8_scalars_and_expands_tabs() {
        assert_eq!(display_column("é\tX"), 6);
        assert_eq!(display_column("e\u{301}"), 2);
    }

    #[test]
    fn source_line_renderer_removes_cr_and_escapes_controls() {
        assert_eq!(
            render_source_line("CREATE\tSCHEMA\r"),
            "CREATE    SCHEMA".to_owned()
        );
        assert_eq!(
            render_source_line("bad\u{0007}\u{2028}\u{2029}\u{202E}\u{200B}"),
            "bad\\u{0007}\\u{2028}\\u{2029}\\u{202E}\\u{200B}".to_owned()
        );
    }

    #[test]
    fn character_boundary_helpers_never_split_utf8() {
        let source = "éclair";
        assert_eq!(char_boundary_at_or_before(source, 1), 0);
        assert_eq!(char_boundary_at_or_after(source, 1), 2);
        assert_eq!(char_boundary_at_or_before(source, 3), 3);
    }

    #[test]
    fn long_source_lines_keep_the_marker_in_a_bounded_window() {
        let raw = format!("{}target{}", "a".repeat(180), "z".repeat(80));
        let (line, marker_start, marker_width) = clip_source_line(&raw, 180, 6);

        assert!(line.starts_with("..."));
        assert!(line.ends_with("..."));
        assert!(line.len() <= 126);
        assert!(marker_start < line.len());
        assert_eq!(marker_width, 6);
        assert_eq!(&line[marker_start..marker_start + marker_width], "target");
    }

    #[test]
    fn multiline_annotations_bound_middle_context() {
        assert_eq!(
            selected_source_lines(2, 10),
            vec![Some(2), Some(3), None, Some(10)]
        );
    }
}

#[cfg(test)]
mod escaping_tests {
    use super::*;

    #[test]
    fn escapes_each_message_scalar_once() {
        let mut output = Vec::new();
        write_escaped_message(&mut output, "a\\b\n\r\t\u{001b}\u{2028}\u{2029}é")
            .expect("Vec accepts every write");
        assert_eq!(
            output,
            "a\\\\b\\n\\r\\t\\u{001B}\\u{2028}\\u{2029}é".as_bytes()
        );
    }
}
