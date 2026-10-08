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
    "  import BUNDLE --dry-run [--limit N] [--quiet] [--type MEDIA_TYPE] [--format human|table]",
    "  import exit status: 0 report printed; 1 bundle missing, unverifiable, or flag invalid",
    "Editor tooling:",
    "  semantic-legend [--json|--markdown|--format csv|--quiet] [--limit N]",
    "  semantic-legend                  token types and modifiers as text",
    "  semantic-legend --limit 5        first five token types (default: all token types)",
    "  semantic-legend --quiet          bare names, one per line, for scripts",
    "  semantic-legend --markdown       legend table with hover samples",
    "  semantic-legend --format csv     kind,index,name,sample rows",
    "  semantic-legend exit codes: 0 legend printed; 2 unsupported option or invalid --limit value",
    "Exit status:",
    "  0 success            1 target             2 usage              3 connection",
    "  4 authorisation      5 presentation       6 cancelled          7 protocol",
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
        assert!(HELP_LINES.contains(
            &"  import BUNDLE --dry-run [--limit N] [--quiet] [--type MEDIA_TYPE] [--format human|table]"
        ));
        assert!(HELP_LINES.contains(
            &"  import exit status: 0 report printed; 1 bundle missing, unverifiable, or flag invalid"
        ));
        assert!(HELP_LINES.contains(&"  status [--porcelain|--short|--format human|short|json]"));
    }

    #[test]
    fn help_documents_every_semantic_legend_mode_with_an_example() {
        let index = HELP_LINES
            .iter()
            .position(|line| *line == "Editor tooling:")
            .expect("editor tooling group");
        let group = &HELP_LINES[index + 1..];
        assert!(HELP_LINES.contains(
            &"  semantic-legend --limit 5        first five token types (default: all token types)"
        ));
        assert!(
            group
                .iter()
                .any(|line| line.contains("exit codes: 0 legend printed; 2 unsupported"))
        );
        for mode in ["--json", "--markdown", "--format csv", "--quiet", "--limit"] {
            assert!(
                group.iter().any(|line| line.contains(mode)),
                "help lacks an example for {mode}"
            );
        }
    }
}
