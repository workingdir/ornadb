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
    "  push [REMOTE] [BRANCH]        publish the branch with its continuity refs",
    "  push exit status: 0 pushed; 1 repository, remote, or continuity conflict",
    "  serve [--port PORT]",
    "  mount DIR --at SELECTOR       read-only view of one resolved snapshot",
    "  mount status [--json]         report attached read-only views",
    "  unmount DIR                   release a view's record",
    "  mount exit codes: 0 attached, released or reported; 1 snapshot, mountpoint or record unavailable",
    "  diff [GIT_DIFF_ARGS...]",
    "  log [GIT_LOG_ARGS...]",
    "  import BUNDLE --dry-run [--limit N] [--quiet] [--metadata-only] [--type MEDIA_TYPE] [--format human|table]",
    "  import exit status: 0 report printed; 1 bundle missing, unverifiable, or flag invalid",
    "  export DEST --at SELECTOR [--at SELECTOR]... [--dependency PATH=DIR]...",
    "  export ARCHIVE --check                         report what one archive records",
    "  export ARCHIVE --restore DEST [--worktree]     reconstruct, verify, then materialise",
    "  export exit status: 0 archive written, verified, or restored; 1 snapshot, object closure, or pinned dependency unavailable",
    "  publish [--message MESSAGE]                    commit the durable runtime tail as one publication commit",
    "  publish exit status: 0 published or nothing to publish; 1 repository, runtime, or publication conflict",
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
    "History commands:",
    "  history RELATION KEY [--at SELECTOR] [--limit N] [--since COMMIT] [--format human|json] [--reverse] [--author TEXT] [--count]",
    "  history exit codes: 0 listed or counted; 1 any history error (bad flag or value, unknown repository, row, --at selector, or --since commit)",
    "Query commands:",
    "  query RELATION [--key KEY|0xBYTES] [--field N] [--limit N] [--format human|json]",
    "  query                            Blob metadata of committed rows, payload-free",
    "  query exit codes: 0 metadata listed; 1 any query error (bad flag or value, unknown repository or relation)",
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
        assert!(HELP_LINES.contains(&"  log [GIT_LOG_ARGS...]"));
        assert!(HELP_LINES.contains(
            &"  import BUNDLE --dry-run [--limit N] [--quiet] [--metadata-only] [--type MEDIA_TYPE] [--format human|table]"
        ));
        assert!(HELP_LINES.contains(
            &"  export DEST --at SELECTOR [--at SELECTOR]... [--dependency PATH=DIR]..."
        ));
        assert!(HELP_LINES.contains(
            &"  import exit status: 0 report printed; 1 bundle missing, unverifiable, or flag invalid"
        ));
        assert!(HELP_LINES.contains(&"  status [--porcelain|--short|--format human|short|json]"));
    }

    #[test]
    fn help_documents_history_flags_and_exit_codes() {
        let index = HELP_LINES
            .iter()
            .position(|line| *line == "History commands:")
            .expect("history group");
        let group = &HELP_LINES[index + 1..];
        for flag in [
            "--at",
            "--limit",
            "--since",
            "--format",
            "--reverse",
            "--author",
            "--count",
        ] {
            assert!(
                group
                    .iter()
                    .any(|line| line.starts_with("  history ") && line.contains(flag)),
                "help lacks history flag {flag}"
            );
        }
        assert!(
            group
                .iter()
                .any(|line| line.starts_with("  history exit codes: 0 ")
                    && line.contains("; 1 any history error"))
        );
    }

    #[test]
    fn help_documents_query_flags_and_exit_codes() {
        let index = HELP_LINES
            .iter()
            .position(|line| *line == "Query commands:")
            .expect("query group");
        let group = &HELP_LINES[index + 1..];
        for flag in ["--key", "--field", "--limit", "--format"] {
            assert!(
                group
                    .iter()
                    .any(|line| line.starts_with("  query ") && line.contains(flag)),
                "help lacks query flag {flag}"
            );
        }
        assert!(
            group
                .iter()
                .any(|line| line.starts_with("  query exit codes: 0 ")
                    && line.contains("; 1 any query error"))
        );
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
