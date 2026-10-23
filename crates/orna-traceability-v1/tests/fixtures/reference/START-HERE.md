# Orna 1.1.0 — final consolidated plan and reference

**Publication revision: `final-2026-10-05`.** Pin this archive's checksum.
The requested language version stays 1.1.0; the new repository writer is
**format 3**. This replaces the earlier 1.1.0 authoring draft, rather than
stacking an addendum on its conflicting storage rules.

**Conceptual integrity:** code defines the database, one indexed store holds
its rows, immutable Blob graphs hold content, Git versions them together,
and the VFS/devtools are interfaces to that same database.

## Give the implementation orchestrator this

```text
Read reference/Orna-1.1.0/START-HERE.md and release.json first.
Then read implementation/ORCHESTRATOR.md and the complete ordered reference
in source/manifest.json. Execute that goal against the existing checkout.
This exact publication replaces earlier Orna 1.1.0 authoring drafts.
Do not implement the old loose/compact/hybrid writers or bytes-only new Blob
semantics from compatibility/previous-1.1.0-draft.zip.
Root: openai-codex/gpt-5.6-terra.
Every descendant and reviewer: openai-codex/gpt-5.6-luna, maximum depth 3.
```

## Reading map

**PLAN.md** is the developer decision brief. **WALKTHROUGH.md** shows rows,
mutations, physical paths and writable/history mounts through examples.
**Orna-1.1.0.md** and **index.html** are the complete assembled reference.
**profiles/** holds exact storage/codec/MIME/VFS contracts. **api/sys.json**
is the callable system inventory; retired storage controls are explicit.
**MIGRATION.md** and **implementation/** contain ordering, domain ownership,
acceptance gates and the continuous implementation goal.

**Evidence is scoped.** `evidence/VALIDATION.md` records checks actually run
for this publication. Orna engine, FUSE/editor compatibility, crash recovery
and 10 GB/million-row performance remain implementation tests, not claims
made by the Python reference models or by publishing this ZIP.

The final artifact resolves design choices; it does not certify an unbuilt
engine or promise that unknown bugs cannot exist. A discovered contradiction
must be recorded and fixed with versioned evidence, never quietly bypassed.
