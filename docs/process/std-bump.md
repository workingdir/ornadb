# Standard library bump

This is the exact procedure for moving the `stdlib/std` gitlink to a new
commit and regenerating the artefacts that pin it. The rules behind each step
are in the [maintainer runbook](../maintainer-runbook.md#standard-library-submodule-bump).

`scripts/bump_std.py` does the regeneration. It never moves the submodule and
never edits the gitlink. Run every command from the ornadb worktree root.

## Commands

1. Check the target out inside the submodule. The commit must be reachable
   from `ornadb-std` `main`; the pin checker enforces that in step 4.

   ```sh
   git -C stdlib/std fetch --depth 1 origin <sha>
   git -C stdlib/std checkout --detach <sha>
   ```

2. Dry run. This is the default and writes nothing. It prints the mirror rows
   to add and remove, the module count change, the pin change, the test count
   assertions to rewrite, and any remaining old-pin mentions.

   ```sh
   python3 -m scripts.bump_std
   ```

3. Apply. It refuses unless `stdlib/std` is exactly the target and has no local
   changes. It writes the mirror, the evaluator count, the test count
   assertions and `CURRENT_STD_PIN`, then runs
   `scripts/check_fixture_manifest.py --update`.

   ```sh
   python3 -m scripts.bump_std --apply
   ```

4. Stage the gitlink and verify.

   ```sh
   git add stdlib/std
   python3 scripts/check_standard_library_pin.py --check
   python3 -m unittest scripts.test_standard_library_pin scripts.test_bump_std scripts.test_fixture_manifest
   python3 scripts/check_fixture_manifest.py --check
   ```

5. Run the focused Rust proof that covers the snapshot you changed, for example
   `cargo test --locked -p orna-evaluator --test standard_profile_v1`. The script
   does not run it.

6. Update `CHANGELOG.md` and any old-pin mention the dry run listed. The script
   reports those and does not rewrite them.

## Exit status

- `0`: the dry run planned no changes (or the apply succeeded).
- `1`: blocked. The report names the invariant. Fix it before applying.
- `2`: usage or precondition failure, such as an uninitialised submodule, an
  unknown commit, or a dirty or mismatched checkout.

## Blockers

The dry run and the apply both stop, with exit `1`, when:

- a removed module's constant is still used by another Rust source;
- a new module's constant name already names a different module;
- the evaluator's `[(String, String); N]` literal disagrees with the mirror row
  count;
- an entry in `UNMIRRORED_SOURCES` names a file that is no longer tracked;
- the mirror that would be written does not list exactly the tracked,
  non-excused sources.

Resolve the blocker in the same change. Do not rename a constant only to get
past it.

## Anti-patterns

- Using the script to move the submodule. It never fetches, checks out, or
  edits the gitlink; it only reads the checkout you prepared in step 1. Do not
  use `git submodule update --remote`; use the step 1 commands.
- Hand-editing mirror rows, `N` in `[(String, String); N]`, test count
  assertions, or `CURRENT_STD_PIN`. The checker and the manifest will disagree
  with the bytes.
- Editing `scripts/fixture-manifest.sha256` by hand. Only
  `check_fixture_manifest.py --update` writes it.
- Applying on a dirty or mismatched checkout, or working around the refusal.
- Pinning a commit that is not on `ornadb-std` `main`.
- Treating the `orna.std/N` identifiers or the `ACCEPTED_*_SOURCE_REVISION_DIGEST`
  constants as part of a stdlib bump. The runbook explains why they are not.

## Known limits

- `--apply` writes the artefacts before it refreshes the fixture manifest. If
  the refresh fails, it exits `1` and the artefact edits remain. Inspect them
  with `git status` and `git diff`, fix the cause, and re-run
  `python3 scripts/check_fixture_manifest.py --update`.
- New mirror constants follow the existing `REFERENCE_STANDARD_<STEM>_PATH_V1`
  pattern for plain stems. Nested paths such as `io/main.orna` do not follow a
  single rule. The script refuses a name clash but not an unusual name, so
  review the `+` lines of the dry run.
