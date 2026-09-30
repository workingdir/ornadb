# 11. Stream programs and consumer identity {#streams}

## Producing and consuming streams

A stream is a sequence whose next value may require waiting or external work. A relation is a queryable value over a selected database state. Neither starts merely because its type or module is loaded.

A finite, self-contained example uses a list-backed source:

```orna
pub fn ingest(samples: Stream<Sample>) =
    samples | for_each(sample => {
        Reading.insert({
            sensor: sample.sensor,
            sequence: sample.sequence,
            value: sample.value,
        });
    });
```

The [sensor project](#examples) defines `Sample`, `Reading` and an offline source with a replay position. Each callback is its own activation; the function returns when the finite source closes. An unbounded source keeps the function running until closure, failure or cancellation.

`orna run sensors.ingest` requires its parameters to be supplied through the ordinary invocation binding rules. A zero-argument `main` wrapper may select configuration explicitly. Naming a file `ingest.orna` has no execution effect.

Parallel consumers use ordinary function values when no arguments are captured. A wrapper lambda is needed only to bind arguments or add work. A consumer function has one checkpointed source root; a coordinator may call several separately named consumers, each with its own identity and lease.

## Durable consumer identity

A checkpoint belongs to a durable consumer identity, not a process ID or source line.

Version 1.0 limits one durable consumer function to one checkpointed source root.

**ORNA-CONSUMER-001** Consumer identity is derived from database identity, the stable ObjectId of the invoked public function and canonical invocation arguments.

**ORNA-CONSUMER-002** The connector additionally supplies source identity, partition identity and position-format version. Secret plaintext MUST NOT participate in identity; a stable secret reference name may.

**ORNA-CONSUMER-003** Function code hashes are revisions, not identity. Editing or semantically renaming the function continues from the same checkpoint.

**ORNA-CONSUMER-004** Deleting and recreating a function creates a new consumer identity even when the spelling is reused.

**ORNA-CONSUMER-005** A durable consumer function containing more than one checkpointed source root MUST produce a diagnostic instructing the author to extract separate named consumer functions.

Stateful map/filter logic is replayed from the source in version 1.0. Stateful windows/joins either persist their state in ordinary tables or remain restart-recomputable.

## Source identity

A connector declares whether it is finite/unbounded and replayable, and supplies stable source identity, partition, position format/version, resume behavior and failed-payload preservation/refetch behavior.

**ORNA-CONNECTOR-001** Source identity MUST be based on stable semantic configuration, not memory address, task order or AST position.

## Duplicate consumers

**ORNA-CONSUMER-006** One clone MUST prevent two local processes from concurrently owning the same consumer identity and partition.

**ORNA-CONSUMER-007** Running the same consumer against the same source from independent writable clones is outside the base version 1.0 guarantee unless the connector explicitly defines partitioning or idempotence semantics.

**ORNA-CONSUMER-008** On merge, equal checkpoints merge. Opaque divergent positions create `sys.CheckpointConflict`; Orna MUST NOT choose a seemingly greatest opaque token.

**ORNA-CONSUMER-009** Base Orna cannot detect concurrent ownership of the same consumer in independent clones before they synchronize, because each clone has independent local state and no coordinator. No cross-clone diagnostic is guaranteed. A later checkpoint conflict MAY reveal divergence, but duplicate source reads or external effects may already have occurred.


## Consumption requirements

**ORNA-STREAM-001** Importing or loading a module MUST NOT start a stream.

**ORNA-STREAM-002** A finite stream consumer returns when the source is exhausted.

**ORNA-STREAM-003** An unbounded stream consumer remains running until cancelled, failed or closed.

**ORNA-STREAM-004** Each item or configured ordered batch callback is a separate activation transaction; an unbounded consumer MUST NOT hold one transaction for its lifetime.

**ORNA-STREAM-005** Function values and anonymous functions have identical call semantics when used as stream/concurrency callbacks; wrapper lambdas MUST NOT be required where no arguments are captured.
