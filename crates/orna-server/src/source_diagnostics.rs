//! Shared source diagnostic rendering for source commands.

use std::io::{self, Write};

use orna_compiler::{CompilerDiagnostic, ParseReport};

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
            character if character.is_control() => {
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
    Ok(())
}
fn write_human_diagnostic(
    output: &mut impl Write,
    parse_report: &ParseReport,
    diagnostic: &CompilerDiagnostic,
    colour: bool,
) -> io::Result<()> {
    let location = diagnostic.location();
    let span = location.span();
    let Some(unit) = parse_report
        .units()
        .iter()
        .find(|unit| unit.logical_path() == location.logical_path())
    else {
        return write!(
            output,
            "error[{}]: {}\n  --> {}:{}..{}\n",
            diagnostic.code().as_str(),
            diagnostic.message(),
            location.logical_path(),
            span.start(),
            span.end(),
        );
    };

    let source = unit.source_text();
    let start = span.start().min(source.len());
    let end = span.end().min(source.len()).max(start);
    let start = char_boundary_at_or_before(source, start);
    let end = char_boundary_at_or_after(source, end);
    let line_start = source[..start].rfind('\n').map_or(0, |index| index + 1);
    let line_end = source[start..]
        .find('\n')
        .map_or(source.len(), |offset| start + offset);
    let line_number = source[..line_start]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1;
    let column = display_column(&source[line_start..start]) + 1;
    let line = &source[line_start..line_end];
    let source_line = render_source_line(line);
    let underline_end = end.min(line_end);
    let underline_width = display_column(&source[start..underline_end]).max(1);
    let eof = end == source.len() && start == source.len();
    let marker = format!(
        "{}{}{}",
        " ".repeat(display_column(&source[line_start..start])),
        "^".repeat(underline_width),
        if eof { " EOF" } else { "" }
    );
    if colour {
        write!(output, "\x1b[1;31merror\x1b[0m")?;
    } else {
        write!(output, "error")?;
    }
    writeln!(
        output,
        "[{}]: {}",
        diagnostic.code().as_str(),
        diagnostic.message()
    )?;
    if colour {
        writeln!(
            output,
            "  \x1b[36m-->\x1b[0m {}:{}:{}",
            location.logical_path(),
            line_number,
            column
        )?;
    } else {
        writeln!(
            output,
            "  --> {}:{}:{}",
            location.logical_path(),
            line_number,
            column
        )?;
    }
    output.write_all(b"   |\n")?;
    writeln!(output, "{line_number:>2} | {source_line}")?;
    if colour {
        writeln!(output, "   | \x1b[31m{marker}\x1b[0m")?;
    } else {
        writeln!(output, "   | {marker}")?;
    }
    if let Some(help) = help_for(diagnostic) {
        if colour {
            writeln!(output, "   |\n   = \x1b[1;32mhelp\x1b[0m: {help}")?;
        } else {
            writeln!(output, "   |\n   = help: {help}")?;
        }
    }
    Ok(())
}
fn render_source_line(line: &str) -> String {
    line.chars()
        .flat_map(|character| match character {
            '\t' => "    ".chars().collect::<Vec<_>>(),
            '\r' => Vec::new(),
            character if character.is_control() => {
                format!("\\u{{{:04X}}}", character as u32).chars().collect()
            }
            character => vec![character],
        })
        .collect()
}
fn help_for(diagnostic: &CompilerDiagnostic) -> Option<&'static str> {
    if diagnostic.code().as_str() == "ORNA0001" && diagnostic.message().contains("schema name") {
        return Some("write a schema name before the semicolon");
    }
    diagnostic.code().help()
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

#[cfg(test)]
mod tests {
    use super::*;
    use orna_core::source::{SourceBundle, SourceUnit};
    use orna_compiler::CompilerDiagnostic;

    fn broken_report() -> orna_compiler::StandardApplicationCheckReport {
        let source = SourceBundle::new([SourceUnit::new("main.orna", "CREATE SCHEMA ;")])
            .expect("source bundle");
        let standard = orna_compiler::check_standard_library_source(
            &orna_standard::retained_standard_library_v10_snapshot()
                .and_then(orna_standard::verify_standard_library_v10_snapshot)
                .expect("standard snapshot"),
        )
        .expect("standard source");
        orna_compiler::check_new_application(&source, &standard).expect("source check")
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
        let source = SourceBundle::new([SourceUnit::new("main.orna", "bad\u{0007}x")])
            .expect("source bundle");
        let standard = orna_compiler::check_standard_library_source(
            &orna_standard::retained_standard_library_v10_snapshot()
                .and_then(orna_standard::verify_standard_library_v10_snapshot)
                .expect("standard snapshot"),
        )
        .expect("standard source");
        let report = orna_compiler::check_new_application(&source, &standard).expect("source check");
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
        assert_eq!(marker, "   | ^^^");
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
        assert_eq!(render_source_line("bad\u{0007}"), "bad\\u{0007}".to_owned());
    }

    #[test]
    fn character_boundary_helpers_never_split_utf8() {
        let source = "éclair";
        assert_eq!(char_boundary_at_or_before(source, 1), 0);
        assert_eq!(char_boundary_at_or_after(source, 1), 2);
        assert_eq!(char_boundary_at_or_before(source, 3), 3);
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
