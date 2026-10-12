//! User-facing command taxonomy and help rendering.

/// Stable help lines for the currently implemented CLI surface.
///
/// Repository maintenance stays visible as its own group so users can find
/// recovery-oriented operations without turning internal runtime machinery
/// into top-level command families.
pub(super) const HELP_LINES: &[&str] = &[
    "Orna commands:",
    "  repl [EXPRESSION]",
    "  run [QUALIFIED_FUNCTION]",
    "  check",
    "  invoke TARGET",
    "  explain CODE",
    "Repository and maintenance commands:",
    "  init [DIRECTORY]",
    "  status [--porcelain|--short|--format human|short|json]",
    "  --format human|short|json status",
    "  fetch [REMOTE] [BRANCH]",
    "  serve [--port PORT]",
    "  diff [GIT_DIFF_ARGS...]",
    "Options: --color auto|always|never, --db ENDPOINT, --debug (show technical detail)",
];

pub(super) fn print_help() {
    println!("orna-cli-v1 [OPTIONS] [COMMAND]");
    for line in HELP_LINES {
        println!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::HELP_LINES;

    #[test]
    fn help_groups_orna_work_and_repository_maintenance() {
        assert_eq!(HELP_LINES[0], "Orna commands:");
        assert!(HELP_LINES.contains(&"Repository and maintenance commands:"));
        assert!(HELP_LINES.contains(&"  fetch [REMOTE] [BRANCH]"));
        assert!(HELP_LINES.contains(&"  diff [GIT_DIFF_ARGS...]"));
        assert!(HELP_LINES.contains(&"  status [--porcelain|--short|--format human|short|json]"));
    }
}
