# Work ADR 0103: Studio Runtime and Inspector Reference Boundary

**Status:** Orna 1.0 inspection boundary accepted; Studio-specific runtime and explorer deferred

## Decision

Orna 1.0 defines generic value inspection and presentation behavior that any
Studio renderer must preserve. Structural Inspect exposes type and structure,
may truncate large collections, and redacts secret values
(**ORNA-PRES-002**, `source/13-presentation.md:25`). Its host-derived fallback
is cycle-safe and bounded (**ORNA-PRES-006**, `source/13-presentation.md:95`).
Presenter failures and unknown rich nodes fall back to Inspect-compatible
output (**ORNA-PRES-009**, `source/13-presentation.md:101`; **ORNA-PRES-010**,
`source/13-presentation.md:117`). Display and Present remain read-only,
deterministic within immutable context, and budgeted
(**ORNA-PRES-008**, `source/13-presentation.md:99`).

Secret contents remain redacted from Inspect, Display, Present, diagnostics,
traces, and `sys` (**ORNA-SECRET-002**, `source/27-secrets.md:9`). The
`sys.Secret` surface exposes metadata such as name, provider, and availability,
not the secret value (**ORNA-SECRET-004**, `source/27-secrets.md:37`). A
coherent multi-relation inspection pins and returns one observation snapshot
(**ORNA-SYS-091**, `source/15-system.md:329`); pruned detail is unavailable
metadata, not an empty result (**ORNA-SYS-094**, `source/15-system.md:335`).
When installed at `/`, the default frontend SHOULD be an ordinary Orna
application, preferably `std.devtools`, and SHOULD lead with database tables
while exposing the listed repository/runtime areas (**ORNA-SERVE-008** and
**ORNA-SERVE-009**, `source/28-serving.md:31-33`). Presentation trees are
renderer-neutral and renderers use Inspect-compatible fallbacks
(`source/28-serving.md:39`).

These clauses define reusable rendering and observation boundaries; they do
not define a Studio-specific runtime ABI, host adapter, explorer layout,
navigation or interaction model, populated Studio Inspector projection rows,
or capture source for those rows. Defer implementation of the production
Studio runtime and Inspector explorer until those contracts and a production
consumer path are accepted. Do not infer them from generic Present nodes,
page values, or the default frontend recommendation. Existing work ADR 0086
continues to govern the deferral of populated Inspector projection rows; work
ADR 0100 governs the separate Studio source-tooling boundary.

## Search evidence

The following search was run over the four named Orna 1.0 source chapters:

```sh
rg -n -i 'Studio|Inspector explorer|Studio runtime|explorer UI' \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/source/15-system.md \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/source/27-secrets.md \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/source/13-presentation.md \
  /home/pbox/dev/ornadb/reference/Orna-1.0.0/source/28-serving.md
```

It returned no matches (exit 1). A repository filename search for `*studio*`
and `*inspector*` used this command:

```sh
rg --files -g '*studio*' -g '*inspector*'
```

It found `docs/inspector-projection-issue-10-reference-disposition.md`, work
ADRs 0080, 0081, 0086 and 0100, this ADR, and
`crates/orna-client/examples/studio_demo.rs`; it found no Studio application
tree or production Studio runtime. The normative generic behavior above is
accepted; Studio-specific UI and runtime behavior remains non-normative and
deferred.

## Precedence

The frozen OrnaDB 1.0.0 reference is authoritative. This ADR records its
generic inspection/presentation constraints and the Studio-specific deferral;
it adds no runtime ABI, Inspector schema, UI behavior, or security guarantee.
