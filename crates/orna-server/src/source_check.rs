//! Offline checking for one new-application source file.

use std::{fs, io::Write, os::unix::fs::OpenOptionsExt};

use orna_compiler::{
    NewApplicationCheckError, check_new_application, check_standard_library_source,
};
use orna_core::source::{SourceBundle, SourceUnit};
use orna_standard::{
    retained_standard_library_v6_snapshot, verify_standard_library_v6_snapshot,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SourceCheckResult {
    Success,
    Failure,
    Usage,
}

pub(super) fn run(path: &str, output: &mut impl Write) -> SourceCheckResult {
    let bytes = match read_regular_file(path) {
        Ok(bytes) => bytes,
        Err(()) => {
            return write_failure(output, &format!("orna: could not read source file: {path}"));
        }
    };
    let source = match String::from_utf8(bytes) {
        Ok(source) => source,
        Err(_) => {
            return write_failure(
                output,
                &format!("orna: source file is not valid UTF-8: {path}"),
            );
        }
    };
    let bundle = match SourceBundle::new([SourceUnit::new(path, source)]) {
        Ok(bundle) if bundle.len() == 1 => bundle,
        _ => return SourceCheckResult::Usage,
    };
    let snapshot = match retained_standard_library_v6_snapshot()
        .and_then(verify_standard_library_v6_snapshot)
    {
        Ok(snapshot) => snapshot,
        Err(_) => return write_standard_failure(output),
    };
    let standard = match check_standard_library_source(&snapshot) {
        Ok(standard) => standard,
        Err(_) => return write_standard_failure(output),
    };
    let report = match check_new_application(&bundle, &standard) {
        Ok(report) => report,
        Err(NewApplicationCheckError::SourceUnitCount { .. }) => {
            return SourceCheckResult::Usage;
        }
        Err(
            NewApplicationCheckError::Catalogue { .. } | NewApplicationCheckError::Context { .. },
        ) => return write_standard_failure(output),
        Err(_) => return write_standard_failure(output),
    };
    if report.diagnostics().is_empty() {
        return SourceCheckResult::Success;
    }
    for diagnostic in report.diagnostics() {
        let location = diagnostic.location();
        let span = location.span();
        if writeln!(
            output,
            "{}:{}..{}: {}: {}",
            location.logical_path(),
            span.start(),
            span.end(),
            diagnostic.code().as_str(),
            escape_message(diagnostic.message()),
        )
        .is_err()
        {
            return SourceCheckResult::Failure;
        }
    }
    SourceCheckResult::Failure
}

fn read_regular_file(path: &str) -> Result<Vec<u8>, ()> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| ())?;
    if !file.metadata().map_err(|_| ())?.is_file() {
        return Err(());
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes).map_err(|_| ())?;
    Ok(bytes)
}

fn write_standard_failure(output: &mut impl Write) -> SourceCheckResult {
    write_failure(
        output,
        "orna: embedded standard library could not be verified",
    )
}

fn write_failure(output: &mut impl Write, message: &str) -> SourceCheckResult {
    let _ = writeln!(output, "{message}");
    SourceCheckResult::Failure
}

fn escape_message(message: &str) -> String {
    let mut escaped = String::with_capacity(message.len());
    for character in message.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{2028}' | '\u{2029}' => {
                use std::fmt::Write as _;
                let _ = write!(escaped, "\\u{{{:04X}}}", character as u32);
            }
            character if character.is_control() => {
                use std::fmt::Write as _;
                let _ = write!(escaped, "\\u{{{:04X}}}", character as u32);
            }
            character => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_each_message_scalar_once() {
        assert_eq!(
            escape_message("a\\b\n\r\t\u{001b}\u{2028}\u{2029}é"),
            "a\\\\b\\n\\r\\t\\u{001B}\\u{2028}\\u{2029}é"
        );
    }
}
