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
    "  mv OLD NEW                    rename a table, field, type, variant or unit, keeping its identity",
    "  status [--porcelain|--short|--format human|short|json]",
    "  --format human|short|json status",
    "  fetch [REMOTE] [BRANCH]",
    "  push [REMOTE] [BRANCH]        publish the branch with its continuity refs",
    "  push exit status: 0 pushed; 1 repository, remote, or continuity conflict",
    "  serve [--port PORT] [--allow-push]  serve this clone on loopback; Git push needs --allow-push",
    "  mount DIR --cwd              editable workspace view; row saves are validated",
    "  edit PATH                     edit one projected row or Blob through transactional admission",
    "  edit --row PATH               edit a row through the same transactional admission path",
    "  mount DIR --at SELECTOR       read-only view of one resolved snapshot",
    "  mount status [--json]         report attached views",
    "  unmount DIR                   detach the kernel mount (orphaned Orna mounts too) and release its view record",
    "  mount exit codes: 0 attached, released or reported; 1 snapshot, mountpoint or record unavailable",
    "  diff [GIT_DIFF_ARGS...]",
    "  log [GIT_LOG_ARGS...]",
    "  branch [NAME]                 list local branches, or create NAME at HEAD with no commit",
    "  annotate FILE --type MEDIA_TYPE --output FILE [--suffix HINT]  write an annotated Blob file",
    "  annotate exit status: 0 annotated Blob file written; 1 file or annotation failure; 2 invalid arguments or format size limit",
    "  import BUNDLE --dry-run [--limit N] [--quiet] [--metadata-only] [--type MEDIA_TYPE] [--format human|table]",
    "  import exit status: 0 report printed; 1 bundle missing, unverifiable, or flag invalid",
    "  export DEST --at SELECTOR [--at SELECTOR]... [--dependency PATH=DIR]...",
    "  export ARCHIVE --check                         report what one archive records",
    "  export ARCHIVE --restore DEST [--at COMMIT] [--worktree] reconstruct, verify, then materialise",
    "  export exit status: 0 archive written, verified, or restored; 1 snapshot, object closure, or pinned dependency unavailable",
    "  migrate copy DEST --annotation-defaults N --no-persistent-consumers",
    "  prepares a journalled candidate in DEST; the format-1/2 source stays unchanged",
    "  requires a decoded annotation count and --no-persistent-consumers, which confirms that consumer checkpoints are carried, not reset",
    "  refuses a source with live stream state; does not activate format 3",
    "  migrate copy exit status: 0 prepared/recovered; 1 migration conflict; 2 missing or invalid coordinates",
    "  migrate abort DEST",
    "  discards the prepared format-3 copy in DEST after verifying it; the source stays unchanged",
    "  migrate abort exit status: 0 discarded or nothing to discard; 1 DEST is not the prepared copy of this source, the source changed before the copy was quarantined, DEST and its quarantine both exist, or the local worktree is unavailable; 2 missing or invalid arguments",
    "  publish [--message MESSAGE]                    commit the durable runtime tail as one publication commit",
    "  publish exit status: 0 published or nothing to publish; 1 repository, runtime, or publication conflict",
    "  commit -m MESSAGE                           review and publish exactly the staged candidate",
    "  commit exit status: 0 committed; 1 candidate validation or stale-review conflict",
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
    "  history RELATION --diff FROM TO    what changed between two pinned snapshots, payload-free",
    "  history exit codes: 0 listed or counted; 1 any history error (bad flag or value, unknown repository, row, --at selector, or --since commit)",
    "Query commands:",
    "  query RELATION [--key KEY|0xBYTES] [--field N] [--at SELECTOR] [--limit N] [--format human|json]",
    "  query                            Blob metadata of committed rows, payload-free",
    "  query --at SELECTOR              the same listing read from that commit, not HEAD",
    "  query exit codes: 0 metadata listed; 1 any query error (bad flag or value, unknown repository, relation, or --at selector)",
    "Options: --color auto|always|never, --db ENDPOINT, --debug (show technical detail)",
];

/// Usage line printed before the help groups; the installed command is `orna`.
const USAGE_LINE: &str = "orna [OPTIONS] [COMMAND]";

pub(super) fn print_help() {
    println!("{USAGE_LINE}");
    for line in HELP_LINES {
        println!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::{HELP_LINES, USAGE_LINE};

    #[test]
    fn help_names_orna_and_describes_current_command_effects() {
        assert_eq!(USAGE_LINE, "orna [OPTIONS] [COMMAND]");
        assert!(
            !HELP_LINES.iter().any(|line| line.contains("OVB")),
            "normal help must not name wire encodings (ORNA-UX-002)"
        );
        assert!(
            HELP_LINES
                .iter()
                .any(|line| line.starts_with("  unmount DIR ")
                    && line.contains("detach the kernel mount")
                    && line.contains("release its view record"))
        );
        assert!(
            HELP_LINES
                .iter()
                .any(|line| line.contains("--no-persistent-consumers")
                    && line.contains("consumer checkpoints are carried, not reset"))
        );
        assert!(
            !HELP_LINES
                .iter()
                .any(|line| line.contains("no-consumer choice"))
        );
    }

    #[test]
    fn help_groups_orna_work_and_repository_maintenance() {
        assert_eq!(HELP_LINES[0], "Orna commands:");
        assert!(HELP_LINES.contains(&"Repository and maintenance commands:"));
        assert!(
            HELP_LINES
                .iter()
                .any(|line| line.starts_with("  mv ") && line.contains("table"))
        );
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
        assert!(
            HELP_LINES
                .contains(&"  migrate copy DEST --annotation-defaults N --no-persistent-consumers")
        );
        assert!(HELP_LINES.contains(&"  migrate abort DEST"));
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
            "--diff",
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
        for flag in ["--key", "--field", "--at", "--limit", "--format"] {
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
