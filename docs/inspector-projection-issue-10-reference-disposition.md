# Inspector projection reference disposition

**Disposition:** Defer populated Inspector resource and UI projections as an
Orna 1.0.0 conformance requirement. The frozen reference does not specify
their row schemas or capture semantics.

## Reference review

The audit covered the normative index `Orna-1.0.0.md`, all `source/` chapters,
`grammar/`, `tests/` (including requirement/evidence registries), `examples/`,
and the API/system reference.

| Reference | Requirement | Defined behavior |
| --- | --- | --- |
| `source/13-presentation.md` | `ORNA-PRES-002`, `ORNA-PRES-006` | Inspect exposes value type and structure, may truncate large collections, redacts secrets, and provides a bounded host fallback. |
| `source/13-presentation.md` | `ORNA-PRES-007`, `ORNA-PRES-009`, `ORNA-PRES-010` | Present returns the core typed presentation tree; presenter failures and unknown rich nodes fall back to Inspect-compatible output. |
| `source/15-system.md` | `ORNA-SYS-035`, `ORNA-SYS-091`, `ORNA-SYS-094`, `ORNA-SYS-109` | Source paths are relative or redacted; coherent multi-relation inspection pins one snapshot; pruned detail is unavailable metadata; secret/path redaction applies at Inspect and system boundaries. |
| `source/27-secrets.md` | `ORNA-SECRET-002` | Secret values are redacted from Inspect and other listed boundaries unless disclosure is explicitly privileged. |

`source/30-protocol.md` defines session-scoped page resource handles and the
generic `PresentNode`, renderer, and action protocol. Those rules describe a
live page/watch value; they do not define row identities or population rules
for headless `sys.inspect` projections. `source/34-system-reference.md` and
`api/sys.json` define invocation, stream, and runtime system observations, but
no `sys.inspect` resource/UI projection row schema. The normative index,
grammar, requirement/evidence corpus, and examples also provide no row schema,
capture source, or availability rule for these projections.

The requirements above constrain any Inspector implementation. They do not
require populated resource, stream, UI, surface, node, runtime, or
presentation-candidate rows. Repository-local implementation decisions are
separate from the frozen Orna 1.0.0 normative corpus; this disposition does not
revise those decisions or claim their additional behavior as an `ORNA-*`
requirement.

## Deferred scope

No projection rows or runtime behavior are added by this issue disposition.
The slice is resolved as deferred until a normative reference defines the
projection fields, capture sources, ordering, availability, and redaction
semantics. Existing Inspect structure, bounded fallback, coherent snapshot,
unavailable-detail, and redaction requirements remain in force.
