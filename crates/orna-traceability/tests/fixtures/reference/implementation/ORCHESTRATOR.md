# Orna 1.1.0 final — continuous implementation goal

Read `reference/Orna-1.1.0/START-HERE.md` and `release.json` before changing
implementation. Read all ordered chapters in `source/manifest.json`, the API,
profiles and implementation gates. **This exact final publication is the
implementation authority.** Compatibility archives are old-reader inputs,
not competing goals. Do not silently modify reference files to make code pass.

## Governing principle: conceptual integrity

Keep the executable Git database and its lightweight partial-clone model.
There is one new canonical row representation, one native Git content graph,
one shared transaction/validation engine, and optional UI/VFS projections.
Do not add LFS, a separate media store, a compulsory File/Asset table, hidden
placement preferences, a second type system, a second recovery journal or
application-specific JavaScript masquerading as a platform binding.

The selected `.orna` source is the actual application/source authority. Git
pins the source, schema, data, media and dependency revisions together.
A Blob's raw-byte hash is independent of MIME, filename and physical layout;
its annotated logical value accounts for MIME/hint through versioned codecs.

## Model routing and execution discipline

Root orchestrator: exactly `openai-codex/gpt-5.6-terra`.
Every descendant, nested specialist and independent reviewer: exactly
`openai-codex/gpt-5.6-luna`. No retry/fallback to other models. Root depth 0;
maximum descendant depth 3. Check actual effective routing in the installed
OMP harness. Do not invent configuration keys or start another root via shell.
Missing required model support is a checkpointed blocker, not permission to
substitute another model.

Reuse the user's existing OMP goal-loop and Beads/issue state. After resumption
or compaction, reload the goal, current revision, active workers/owned paths
and pending evidence. A completed batch, isolated helper or plan is not goal
completion. Continue useful authorized work until mandatory integrated gates
are met, a real external blocker prevents progress, or user/resource limits
require a truthful checkpoint. Never turn an unavailable prerequisite into pass.

## Start with the actual repository

Inspect current HEAD, index, dirty files, worktrees, submodules, module/dep graph,
current storage readers/writers and test entrypoints. A historical commit hash
in chat is not the user's current base. Preserve conforming implementation;
this is a controlled storage migration and VFS integration, not a greenfield
rewrite. Preserve user changes and unrelated work. No forced resets, blanket
staging, remote push or history pruning without explicit authorization.

Identify production callers, not merely crates whose names resemble domains.
Avoid a new conformance-owned runtime. Add shared interfaces in production
layers and make tests/adapters consume those interfaces. One worker owns each
shared runtime file, registry, manifest and lockfile at a time. Other workers
may investigate/propose patches; integration is serialized for overlaps.

## Domain ownership

Use these bounded workstreams and nest only when useful:

1. **Values/codecs/MIME:** lazy annotated Blob, exact OVB-2/ROV-3/SOV-3 contexts,
   content-vs-value identity, bounded IO and canonical MIME/suffix policy.
2. **Row/graph store:** OGS-1 native edges, ORP-1 key lookup and overflow, OGB-2
   range trees, indexes, partial-fetch batching and independent extraction.
3. **Transactions/publication/migration:** CAPTURE-3, pins, local atomic writes,
   logical staging, PUB-3/CHECKOUT-3/UPGRADE-3 and crash/continuity recovery.
4. **Writable VFS:** EDIT-1, path/field projection, private candidates, real
   editor save patterns, CAS, coherent open handles/caches and diagnostics.
5. **Minimum devtools/resources:** real table/Run/result/refresh and content
   access through ordinary source-hosted serving; no extra feature programme.
6. **Independent conformance:** adversarial tests, byte/graph verification,
   migration safety, controlled crash tests and cold/warm performance evidence.

Begin with four implementation workers and one independent reviewer where
ownership permits; a proposed global cap is 16 live descendants including
nested leads and review. The cap is an orchestration rule unless the actual
harness enforces it globally. Reserve review/integration capacity. Do not
create many workers that contend for the same giant file or resource budget.

Every assignment states exact requirements/profile clauses, production entry
point, expected artifact/API, owned paths, prerequisite interface, test command
and acceptance result. Require a bounded implementation outcome, not an
open-ended audit. Do not keep growing isolated test families while deferring
integration of the user-facing path.

## Deliver the connected path first

Read `WALKTHROUGH.md`. A real format-3 database must support a Music row with
content Blob; capture a recording/image; query metadata without fetching media;
mount CWD; read data.orna and sibling bytes; update title through Orna; observe
it in the mount; save valid/invalid text through actual editors; replace content;
review/stage/publish; mount a prior commit; reconstruct offline elsewhere.

Writable VFS is part of the requested completed profile. A read-only mount is
an intermediate milestone, not a substitute. Equally, do not claim generic
filesystem atomicity that FUSE does not provide: use the specified save points,
retained drafts and explicit guarded/grouped operations.

Initial UI has no Watch control, snippet store, theme engine or complicated
layout library. Existing language/protocol watchers still retain their rules.
The later Orna-authored browser runtime is a separate profile; do not rewrite
language logic in JS as a shortcut or make that profile block the thin path.

## Final checks and truthful completion

Implement the gate matrix in `ACCEPTANCE.md` using the actual engine, not the
reference Python model as the subject under test. Derive inventory counts from
release data; do not weaken integrity checks to absorb changed counts. Exact
old codec vectors remain fixed. New migrations select versioned schemas and
record stream/consumer identity correspondence rather than silently resetting.

Every completed change includes files, production path, commands/exit codes,
current revision, scope and reviewer findings. Test integrated revision after
merging. Independent review targets lost-response/GC/crash/rename race paths,
not only happy-path parsing. Repeated failed approaches must change, not loop.

Completion requires current-revision evidence for the mandatory repository and
writable VFS gates; optional browser/media claims are explicit. Missing platform,
real editor, scale or fault-injection evidence remains open. Never advertise
this reference package's model checks as runtime conformance. No guarantee of
"airtight" code may substitute for observed proof and documented limitations.
