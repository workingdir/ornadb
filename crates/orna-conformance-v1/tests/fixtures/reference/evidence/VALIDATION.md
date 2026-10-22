# Executed publication checks

Publication: **Orna 1.1.0 / final-2026-10-05**.

| Group | Result | Scope |
|---|---|---|
| current-format-model | PASS | MIME/path/CBOR/CAS/graph model and isolated native Git; not engine/FUSE |
| language-syntax | PASS | Bounded EBNF syntax probe, not compiler/type/effect checking |
| legacy-float-vectors | PASS | Retained legacy numeric model only |
| legacy-path-vectors | PASS | Legacy path reader compatibility, not VFS-1 |
| legacy-live-model | PASS | Retained wire models, not live server interoperability |
| static-publication | PASS | Cross-file source/API/profile/publication consistency |

Current reference-model assertions: 214. Static consistency checks: 208. Full commands/exit codes are in [checks.json](checks.json).

## What this does not prove

No Orna engine build, real FUSE mount, editor compatibility, durable crash recovery, ten-GB import, million-row performance, independent full typed extractor or browser runtime was executed here. Those are explicitly required in ../implementation/ACCEPTANCE.md. The native Git test uses two MiB of synthetic content and places a model map at the store path to test reachability/transport; it is not a complete typed database admission proof. The local CAS model is not SQLite or a FUSE callback implementation. Legacy vectors retain their original context and do not validate the new writer.

The exact previous draft is retained for migration and golden comparisons. Its old evidence is not republished as new results. Re-running checks can change logs/tool versions and therefore the final artifact checksum. Each completed group is recorded immediately; --only GROUP can resume a long validation run, but groups count together only when their complete input fingerprint matches.
