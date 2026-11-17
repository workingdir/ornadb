//! `:doc NAME` and `:source NAME`: the doc comment and the source text of one
//! declaration the admitted project, its attachments, or the standard library
//! declares. Both read the declaration records the project load kept.

use std::io::{self, Write};

use orna_project::DeclarationRecord;

use crate::repl::{AnsiColor, write_styled};

/// Which part of a declaration `:doc` / `:source` prints.
#[derive(Clone, Copy)]
pub(crate) enum Part {
    Docs,
    Source,
}

/// The declaration `name` selects, or the reason it selects none.
///
/// A name that equals a qualified name selects exactly that declaration.
/// Otherwise a bare name selects the declaration whose qualified name ends in
/// `.name`; a name shared by several declarations is ambiguous and is refused
/// with the qualified candidates, so the user can write one of them.
pub(crate) fn find<'a>(
    declarations: &'a [DeclarationRecord],
    name: &str,
) -> Result<&'a DeclarationRecord, String> {
    let exact: Vec<&DeclarationRecord> = declarations
        .iter()
        .filter(|declaration| declaration.qualified_name() == name)
        .collect();
    let matches = if exact.is_empty() {
        let suffix = format!(".{name}");
        declarations
            .iter()
            .filter(|declaration| {
                declaration.name() == name || declaration.qualified_name().ends_with(&suffix)
            })
            .collect()
    } else {
        exact
    };
    match matches.as_slice() {
        [only] => Ok(only),
        [] => Err(format!("no declaration named `{name}` in this session")),
        several => {
            let candidates: Vec<String> = several
                .iter()
                .map(|declaration| declaration.qualified_name())
                .collect();
            Err(format!(
                "`{name}` is ambiguous; it names {}",
                candidates.join(", ")
            ))
        }
    }
}

/// Writes the doc comment lines, or the source text, of `declaration`.
pub(crate) fn write_part<W: Write>(
    writer: &mut W,
    declaration: &DeclarationRecord,
    part: Part,
    color_enabled: bool,
) -> io::Result<()> {
    match part {
        Part::Docs if declaration.docs().is_empty() => {
            write_styled(writer, AnsiColor::Dim, b"(no doc comment)", color_enabled)?;
            writer.write_all(b"\n")
        }
        Part::Docs => {
            for line in declaration.docs() {
                write_styled(writer, AnsiColor::Green, line.as_bytes(), color_enabled)?;
                writer.write_all(b"\n")?;
            }
            Ok(())
        }
        Part::Source => {
            writer.write_all(declaration.source().as_bytes())?;
            writer.write_all(b"\n")
        }
    }
}
