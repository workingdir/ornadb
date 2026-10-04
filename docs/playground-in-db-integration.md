# Playground in-DB integration points

The standalone `playground/` WebAssembly and GitHub Pages iteration has been
removed. The `orna serve` Git listing from S1 remains the starting point. No S2
layout architecture record was present in `docs/decisions/` at this checkout;
this note records integration boundaries without choosing an explorer layout.

## Existing serve surface

`orna serve` keeps the Git transport and the server-rendered Git pages at `/`,
`/tree/<commit>/<path>`, and `/blob/<commit>/<path>`. Its query endpoint is
`POST /api/query`. The authenticated `orna.present.v1` WebSocket presentation
delta transport also remains available. These are the integration seams for
the ordinary Orna application and live presentation described by
ORNA-SERVE-001; Git transport remains usable without an optional frontend.

## Future in-DB application

ORNA-SERVE-008 recommends an ordinary Orna application, preferably
`std.devtools`, as the default frontend. ORNA-SERVE-009 says it should lead
with database tables and expose files, commits, branches, functions,
dependencies, storage, and runtime state. Build those views from introspected
state and renderer-neutral presentation trees. A generic renderer must retain
Inspect-compatible fallback behavior, including redaction and bounded
structural inspection, as described by ORNA-PRES-002, ORNA-PRES-006,
ORNA-PRES-009, and ORNA-PRES-010.

Keep the first presentation content-first: readable plain type, blue links,
minimal CSS, and themeable CSS variables. Add no navigation model or custom
interaction until the S2 application contract calls for it. JavaScript belongs
only at the live presentation boundary needed to consume WebSocket deltas.

Work ADR 0103 records that these generic presentation rules do not specify a
Studio runtime ABI, host adapter, explorer layout, or navigation model. An
accepted S2 architecture record should define any such application-specific
contract before implementation expands beyond these seams.
