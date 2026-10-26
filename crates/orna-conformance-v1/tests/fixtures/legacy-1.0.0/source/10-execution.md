# 10. Activations, transactions and task ownership {#execution}

## Automatic activation transactions

A top-level activation that performs Orna table mutations owns a database transaction. Activation roots include one submitted REPL input, one direct `orna run` invocation, one page action, one stream item or configured batch callback, and one explicitly created concurrent child activation.

**ORNA-TXN-001** Orna-controlled table writes within an activation MUST commit together on successful completion.

**ORNA-TXN-002** If an error, panic or cancellation escapes the activation, all Orna-controlled writes performed by that activation MUST roll back.

**ORNA-TXN-003** Nested ordinary function calls participate in the transaction of the activation root and MUST NOT independently commit.

**ORNA-TXN-004** An unbounded stream program MUST NOT hold one transaction for its lifetime; each item or configured ordered batch callback is a distinct activation.

## Read semantics

**ORNA-TXN-005** An activation MUST capture one starting CWD generation, one snapshot context and one activation time. All reads and repeated function calls in that activation observe that context, `now()` returns the captured activation time, and the activation observes its own successful writes before commit.

**ORNA-TXN-006** Other activations MUST NOT observe partial writes from an uncommitted activation.

## External effects

```text
insert a local row
perform an external HTTP request
insert another local row
```

If the final insert fails, Orna rolls back its database writes but cannot un-send the HTTP request.

**ORNA-TXN-007** Orna MUST NOT imply that external effects are transactionally reversible.

**ORNA-TXN-008** User-visible effect annotations are not required for Orna to identify table mutations it owns. Internal effect information MAY be inferred for diagnostics, planning and introspection.


## Task ownership and termination

An **activation** is the unit of execution and database atomicity. A **task** is scheduled work owned by an activation or session. A task need not be an operating-system process. A **run** is a separately launched program with its own root owner. These identities are distinct from the process currently hosting the embedded runtime.

Ordinary synchronous calls share their caller's activation. Returning from a helper function therefore does not close the activation. Returning from an activation root does. The REPL session is a longer-lived owner: submitting another prompt does not end that session.

**ORNA-CONCUR-001** Every asynchronous child MUST have exactly one activation or session owner. On normal completion, failure, cancellation or loss of that owner, the runtime MUST request cancellation of all unfinished children and join their termination before marking the owner terminal. Normal completion does not wait for unfinished children to complete their intended jobs; it stops them and waits for cleanup.

The ownership rule is lexical at the activation boundary, not based on whether a handle remains reachable. Keeping or discarding an `InvocationHandle` does not detach, reparent or cancel its invocation. A child cannot extend its owner's lifetime by retaining a reference to it.

### Starting and awaiting

`sys.start` returns after validating the target and arguments, allocating the invocation, assigning its owner, and accepting responsibility for termination. Its default transaction mode is `separate`. `read_only` is also permitted. `inherit` fails with `sys.invoke.transaction_mode`: an asynchronously scheduled child may not concurrently use its parent's transaction. Synchronous `sys.invoke` may use `inherit`.

A direct REPL expression or direct binding whose root call is `sys.start(...)`, `sys.admin.retry_failure(...)` or `sys.admin.replay_failure(...)` assigns the invocation to the REPL session. A `sys.start` reached while executing another operation belongs to that operation's activation, including when reached through ordinary helper functions. Host APIs that submit a direct start explicitly identify the session owner; they cannot infer ownership from a later stored handle.

For example, these two REPL inputs may be submitted separately:

```orna
let job = sys.start(worker, arguments, as: Int);
sys.await(job)
```

Here `worker` and `arguments` are already resolved typed values. The invocation remains session-owned between inputs. By contrast, a function that starts work and returns without awaiting it closes its activation by cancelling the unfinished work.

`sys.await` does not transfer ownership. It returns a complete `sys.InvocationResult<T>` when the target terminates. Expiry of its timeout raises `sys.invoke.await_timeout`; it does not itself cancel the target. If that unhandled failure subsequently ends the owner, owner termination cancels its children for that separate reason. A successful result containing an optional `T` retains the outer success wrapper, so success with `null` is distinguishable from absence of a result.

### Ownership termination algorithm TASK-END-1

1. Stop admitting new children under the ending owner. A racing start either joins the recorded child set or fails before executing; it cannot become ownerless.
2. Record the owner's requested completion: success with a value, ordinary failure, or cancellation.
3. Request cancellation of every child that has not already reached a terminal state. Repeat this recursively through their owned descendants.
4. Join each child's termination and release activation-owned resources. Cancellation checks occur at bounded VM, loop, stream and host-call boundaries. An adapter that cannot interrupt an external call must isolate it and prevent it from making further Orna state changes after its lease is revoked.
5. Roll back still-open child transactions. A child's transaction that committed before cancellation remains committed. Cancellation is not history reversal.
6. Complete the owner's own transaction: validate and commit on success, or roll back on failure or cancellation. No child sharing mutable activation resources remains active at this point.
7. Publish terminal invocation and task metadata and release the owner lease. A late completion from a fenced child cannot replace the terminal result or commit another transaction.

**ORNA-TASK-001** A child MUST NOT commit after the runtime has acknowledged its cancellation and terminal state. Lease fencing and transaction generation checks MUST enforce this when execution is hosted in a different process.

**ORNA-TASK-002** A graceful REPL close MUST cancel session-owned invocations and watches without terminating unrelated sessions or independently launched runs in the same runtime.

**ORNA-TASK-003** A hard process termination MUST NOT be described as executing cleanup in the dying process. A surviving runtime cancels work after loss of its owner lease; a restarted runtime recovers or rolls back open transactions before admitting writes. Committed changes remain durable.

**ORNA-TASK-004** The runtime MUST expose bounded cancellation checkpoints and adapter cancellation limitations. A task stuck in untrusted or noncooperative code cannot retain a writable transaction after its ownership lease has been revoked. An implementation may terminate an isolated worker to enforce this rule.

### Concurrent combinators

`std.concurrent` supplies ordinary functions rather than new control-flow syntax. Callback arrays must have one common successful result type; heterogeneous results require an explicitly declared record or enum.

**ORNA-CONCUR-002** `parallel` MUST return successful child results in input order, independent of completion order.

**ORNA-CONCUR-003** Each `parallel` child is a separate activation and transaction. A successful child's committed writes MUST NOT be rolled back merely because another child fails.

**ORNA-CONCUR-004** Programs requiring one atomic database update SHOULD perform concurrent read-only or external work first, then apply the combined table mutations in the parent activation.

**ORNA-CONCUR-005** `race` MUST return the first observed successful result, cancel and join all losers, and choose the lowest input-index ordinary failure if every child fails. Simultaneous successes use the runtime's recorded completion order; no wall-clock ordering between simultaneous events is promised.

**ORNA-CONCUR-006** `timeout` MUST request cancellation and join its child before failing with its timeout diagnostic. It never returns a successful partial value.

For `parallel`, the first observed failure cancels unfinished siblings. After joining, the lowest input-index ordinary failure among failed children is the reported primary cause; other failures are attached as ordered causes. Cancellation requested by the parent remains cancellation and is not converted into an ordinary aggregate failure. An empty `parallel` returns `[]`; an empty `race` fails without scheduling work. Negative timeouts are argument errors; zero timeouts check already-terminal results before cancelling unfinished work.

## Cancellation and bounded streams

**ORNA-CANCEL-001** Cancellation is distinct from ordinary failure and MUST roll back the interrupted activation's open transaction. `|?` cannot catch it.

**ORNA-CANCEL-002** Interpreters, VMs, loops and stream operators MUST check cancellation at bounded execution points.

**ORNA-BACKPRESSURE-001** Stream channels MUST be bounded. A source that cannot be backpressured MUST declare or receive an explicit buffer, drop or fail policy before use.

**ORNA-BACKPRESSURE-002** `for_each` is sequential by default. Concurrent processing MUST be explicit, bounded and consistent with the connector's partition-order and checkpoint rules.

Cancelling a stream callback rolls back both its writes and its checkpoint advance. Stopping the surrounding run prevents admission of the next delivery. Neither action undoes previously committed deliveries.

