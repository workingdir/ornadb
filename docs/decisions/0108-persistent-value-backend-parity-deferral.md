# Work ADR 0108: Defer Persistent Scalar and Record Backend Parity

**Status:** Deferred; this decision accepts no new PostgreSQL value mappings

## Decision

Do not expand PostgreSQL runtime mappings or codecs for the persistable
standard scalars or record values under this task. The OrnaDB 1.0.0 reference
defines logical value and table semantics and a portable physical profile; it
does not define PostgreSQL `RuntimeValue` representations, SQL type choices,
null/default behavior, migration compatibility, or backend record read/write
behavior. Manifest persistence metadata and compact-storage physical encodings
do not decide those PostgreSQL contracts.

This is a no-delta ruling for the PostgreSQL backend. Preserve current mappings
and accepted behavior. Resolve the listed backend questions through the
separate VALUE/OBJECT contract gate before adding mappings or changing
persistability claims.

## Normative boundary

- `ORNA-VALUE-001` (`source/05-types.md:65`) assigns value semantics to
  primitive and record values; it does not select a database representation.
- `ORNA-TABLE-001` (`source/07-tables.md:5`) makes `table` the only core
  declaration for persistent relational data.
- `ORNA-CODEC-003` (`source/13-presentation.md:135`) requires deterministic,
  full-precision, unambiguous, round-trippable canonical Orna encoding for
  supported values; it does not specify a PostgreSQL `RuntimeValue` codec.
- `ORNA-STORAGE-FORMAT-001` and `ORNA-STORAGE-FORMAT-002`
  (`source/23-storage.md:208,210`) require complete physical descriptors and
  canonical logical row/schema identity. The adjacent `compact-storage-v1`
  encoding table (`source/23-storage.md:179-202`) specifies Parquet encodings,
  including scalars and record/tuple groups, not PostgreSQL mappings.

The nearest existing PostgreSQL work contract is work ADR 0046. It accepts
exactly six scalar field additions and explicitly leaves Decimal, UUID, DATE,
TIME, TIMESTAMP, DURATION, and record field additions closed. This ruling does
not amend or broaden that accepted migration scope. Work ADR 0016 also states
that a catalogue-backed `PERSISTABLE` definition does not by itself mean
PostgreSQL creates a relation for every value type.

## Search evidence

On 2026-09-28, this search over the frozen normative bundle roots returned no
matches (exit status 1):

```text
rg -n -i 'PostgreSQL|RuntimeValue|RuntimeValue codec|SQL mapping|record object backend|Postgres backend' /home/pbox/dev/ornadb/reference/Orna-1.0.0/Orna-1.0.0.md /home/pbox/dev/ornadb/reference/Orna-1.0.0/source /home/pbox/dev/ornadb/reference/Orna-1.0.0/api /home/pbox/dev/ornadb/reference/Orna-1.0.0/grammar /home/pbox/dev/ornadb/reference/Orna-1.0.0/tests /home/pbox/dev/ornadb/reference/Orna-1.0.0/examples /home/pbox/dev/ornadb/reference/Orna-1.0.0/profiles
```

No scalar SQL mapping, `RuntimeValue` codec, nullable/default/round-trip rule,
migration rule, or record-value PostgreSQL read/write contract is inferred
from that absence. Cargo tests are not applicable to this documentation-only
decision.
