# Loose row `load_rows` evidence

The frozen `examples/reference/` project has five `.orna` files: `main.orna`,
`library.orna`, `warehouse.orna`, `sensors.orna`, and `values.orna`. All five
are named as modules in its `tests/project-manifest.json`; there are no
additional `.orna` files below the project root, so the frozen project has
zero loose row units. The adjacent `expectations.json` is not an `.orna` row
unit. This matches the complete-project report of `rows=0`.

`ORNA-TEST-003` in `source/32-conformance.md:21` specifies complete-project
loading, discovery of every loose row unit, key reconstruction from paths, and
row field/unit validation against table schemas. `ORNA-TEST-009` at
`source/32-conformance.md:29` requires `load_rows` for complete-project
fixtures.

The new `loose_row_load_rows` integration test supplies checked-in `.orna`
fixtures to the real worktree `ProjectLoader`, analyzes the loaded table
declaration, and passes every discovered candidate to semantic row admission.
It checks a valid two-component key reconstructed from the path and rejects an
invalid integer key component, a field with the wrong type, an unknown field,
and a missing required field. This is evidence for the composed loader and
semantic-admission path only; it does not claim that an Orna engine executed
the project.

Evidence status under `ORNA-TEST-004` (`source/32-conformance.md:23`): the
requirements are specified; the new test exists; the captured focused test run
passed (1 passed, 0 failed, exit 0). The existing frozen reference project
remains marked `implementation_execution: "not executed"`.
