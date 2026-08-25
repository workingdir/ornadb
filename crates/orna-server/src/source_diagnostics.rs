//! Shared source diagnostic rendering for the source commands.

use std::io::{self, Write};

use orna_compiler::CompilerDiagnostic;

/// Renders compiler diagnostics in the stable source-command wire format.
pub(crate) fn render_diagnostics(diagnostics: &[CompilerDiagnostic]) -> Vec<u8> {
    let mut output = Vec::new();
    write_diagnostics(&mut output, diagnostics).expect("writing diagnostics to Vec cannot fail");
    output
}

/// Writes compiler diagnostics in source order, preserving output errors from the destination.
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
