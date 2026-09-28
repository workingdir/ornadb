# CLI usage oracle alignment deferral

Beads issue: `ornadb-1787960948553-29-23b865b4` (GitHub #112)
Inspected base: `origin/main` at `8b4264caf38fd83210419ca24402135d308b1708`
Reference: `/home/pbox/dev/ornadb/reference/Orna-1.0.0`

## Verified reference boundary

`source/18-cli.md:16-29` lists the Orna-specific command names. **ORNA-CLI-001** (`source/18-cli.md:12`) says existing Git operations retain their names, flags, and observable semantics. **ORNA-CLI-002** (`source/18-cli.md:31`) places runtime/checkpoint/failure detail in typed `sys` values and optional `std.devtools` pages.

The frozen CLI source does not define a `runtime describe` command, a global `--help` byte sequence, usage-line order/wrapping, or an exact malformed-command usage oracle. A search for `runtime describe`, `runtime-shared-library`, global usage, usage bytes, and malformed-command in frozen `source/`, `api/`, and `tests/` returned no matches (exit 1; full command and result captured in the transcript).

## Current code and test state

At the inspected base, `crates/orna-cli-v1/src/cli_help.rs` owns `HELP_LINES`; its unit test checks the headings and selected entries, not a separate expected full-output byte string. Current malformed/unknown-command checks assert parser diagnostic fields. Searches across the CLI crate found no `runtime describe` text, `runtime-shared-library` usage, or independent malformed-command global usage oracle. Thus the issue description's stated old byte sequence and current runtime-describe command are not present to align on this base.

The nearest existing checks were executed for bounded evidence:

- `cli_help::tests::help_groups_orna_work_and_repository_maintenance`: 1 passed, exit code 0.
- `cli_args::tests::parser_preserves_option_and_argument_diagnostic_text`: 1 passed, exit code 0.

Full cargo output and exit codes, along with the raw reference/code search output, are in [`cli-usage-oracle-112-search-and-test.log`](cli-usage-oracle-112-search-and-test.log).

## Resolution

Deferred: there is no current independent usage-byte oracle to update, and the frozen reference supplies neither the `runtime describe` command nor exact help bytes. Adding that command or choosing a new byte-for-byte usage contract would invent behavior. No source or test changes are made. The issue can be reopened against a current oracle or an approved exact-help contract; this record resolves only the accepted oracle-alignment slice on the inspected base.
