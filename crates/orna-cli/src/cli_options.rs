//! Command-line words consumed left to right, shared by the command parsers.
//!
//! A parser reports the missing value of an option in its own diagnostic style,
//! so the cursor carries the words and the caller supplies the constructor.

use super::Diagnostic;

/// The command line being read left to right.
pub(super) type Words<'a> = std::iter::Map<std::slice::Iter<'a, String>, fn(&String) -> &str>;

/// Starts reading `arguments` left to right.
pub(super) fn words(arguments: &[String]) -> Words<'_> {
    arguments.iter().map(String::as_str)
}

/// The value one option consumes, or the option's own usage diagnostic.
pub(super) fn value<'a>(
    words: &mut Words<'a>,
    error: impl FnOnce(&'static str, &'static str) -> Diagnostic,
    message: &'static str,
    usage: &'static str,
) -> Result<&'a str, Diagnostic> {
    words.next().ok_or_else(|| error(message, usage))
}
