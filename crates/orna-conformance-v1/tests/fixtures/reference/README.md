# Orna 1.1.0

Start with [START-HERE.md](START-HERE.md).

This is the consolidated specification and implementation plan requested after
the Git-native Blob, single row-store and writable VFS design review. It has
one active source of truth and a checksum-pinned publication identity.

- [Developer plan](PLAN.md)
- [Code/CLI/path walkthrough](WALKTHROUGH.md)
- [Complete reference](Orna-1.1.0.md) or [HTML](index.html)
- [Migration](MIGRATION.md)
- [Implementation orchestrator](implementation/ORCHESTRATOR.md)
- [Evidence](evidence/VALIDATION.md)

Reproduce publication checks with `python3 tools/run_checks.py`. See
`tools/requirements.txt` for authoring dependencies; they are not dependencies
of the Orna engine. Python and Git checks are not engine/FUSE conformance.

Only `compatibility/previous-1.1.0-draft.zip` contains the exact superseded
draft. It is a legacy-reader/golden-vector input, not a competing instruction
file. The output archive must be extracted as `reference/Orna-1.1.0/`; back up
an existing folder before replacement. Do not merge both source editions.

## Reproduce publication checks

Use Python 3.11 or newer, Git, Pandoc and the Python modules in `tools/requirements.txt`. Then run `python3 tools/run_checks.py`. The bounded native-Git fixture uses disposable temporary repositories and synthetic bytes. `--only GROUP` can execute one named group without losing completed matching-input results.

Package with `python3 tools/package_release.py /path/outside/reference/Orna-1.1.0-final.zip`, then verify with `python3 tools/verify_release.py /path/outside/reference/Orna-1.1.0-final.zip`. Regeneration is not implementation conformance.
