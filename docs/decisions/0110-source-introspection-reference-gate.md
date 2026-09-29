# Work ADR 0110: Source Introspection Reference Gate

**Status:** Deferred pending a canonical contract

**Scope:** Beads `ornadb-1787784780769-47-1add7df8` (GitHub #47)

## Ruling

The frozen Orna 1.0.0 reference defines a source-document and object-description
surface. It does not define the separate bounded function-declaration metadata
value requested by #47. This record identifies that boundary; it does not add
a language API, a system identity, or a runtime guarantee.

## What Orna 1.0.0 defines

The system chapter says `sys.source` returns an authored or generated
`sys.SourceDocument` pinned to a file and snapshot, and `sys.describe` returns
structured metadata. It excludes disclosure of credentials, arbitrary host
paths, and unretained historical source (`source/15-system.md:160`). The
system reference defines read-only `sys.source(ObjectRef)` and
`sys.source(FileRef)` overloads returning retained exact source and source maps
subject to redaction (`source/34-system-reference.md:2512-2530`).

`sys.SourceDocument` enumerates the fields `file`, `snapshot`, `text`,
`exact_hash`, `encoding`, `generated`, `maps`, and `unavailable_reason`; its
text/hash and explicit-unavailability invariants are listed at
`source/34-system-reference.md:621-638`. `sys.SourceSpan` defines half-open
UTF-8 byte coordinates and one-based line/column coordinates
(`source/34-system-reference.md:583-599`). `sys.ObjectDescription` contains
the pinned object/revision, kind, qualified name, optional definition/docs/
signature, and `metadata: sys.Value` (`source/34-system-reference.md:664-677`).

Relevant verified requirements are ORNA-LEX-002 (retain positions for line,
column, and byte-span diagnostics; `source/04-lexical.md:7`), ORNA-SYS-034
(semantic and exact-source hashes; `source/15-system.md:134`), ORNA-SYS-035
(repository-relative or redacted paths; `source/15-system.md:166`),
ORNA-SYS-036 (generated definition provenance; `source/15-system.md:136`),
ORNA-SYS-052 (explicitly represent missing promisor data without fabrication;
`source/15-system.md:178`), and ORNA-SYS-128 (exercise source/history overloads
over current, renamed, historical, missing, partially-cloned, and redacted
inputs; `source/15-system.md:367`). ORNA-SYS-043 through ORNA-SYS-045 require
cycle-safe deterministic dependency traversal and preservation of edge kind
and source evidence (`source/15-system.md:150-154`). Those rules define the
dependency APIs; they do not specify an order for a separate source-function
metadata reference list.

## Exact gap

The reference does not name `sys.source.current`, `sys.source.function`,
`SourceFunctionMetadata`, or `ORNA-SOURCE/1`. It does not define a function
declaration metadata record with ordered parameters and references, its
bounded encoding, or its parse/check/artifact/runtime receipt. The
`sys.ObjectDescription.metadata: sys.Value` field is a general value slot; it
does not specify that additional declaration schema or its ordering rules.

Reproduce the name check from the repository root (the reference is a sibling):

```text
rg -n 'sys\.source\.current|sys\.source\.function|SourceFunctionMetadata|ORNA-SOURCE/1' ../reference/Orna-1.0.0/Orna-1.0.0.md ../reference/Orna-1.0.0/source ../reference/Orna-1.0.0/api ../reference/Orna-1.0.0/grammar ../reference/Orna-1.0.0/tests ../reference/Orna-1.0.0/examples --glob '*.md' --glob '*.json' --glob '*.orna' --glob '*.ebnf'
```

The captured result was no matches (exit 1). The exact frozen artifacts define
`sys.source(ObjectRef|FileRef)` and `sys.describe`, but do not define those
additional names or the requested record. In the current repository tree,
`orna-core` already contains `sys.source.current` and its `SourceFunctionMetadata`
carrier. That implementation is not an external 1.0.0 contract receipt.

## Gate to resume

Do not allocate another function or type identity in the standard/system
catalogues, alter the external API inventory, or promote the existing carrier
as portable behavior under the current reference. A canonical contract owner
must first decide whether the additional surface is part of a future Orna
publication or an explicitly versioned implementation extension. That
decision must define the record fields, ordering, source/revision binding,
bounds, unavailable/redacted cases, and required parse, check, artifact,
runtime, and external-contract evidence. Those are open decisions, not
defaults for this implementation to choose.

The production CLIENT VM deferral in Work ADR 0091 is a separate implementation
gate. It does not define or extend source introspection. No Orna source fixture
or runtime test was added in this no-delta resolution.
