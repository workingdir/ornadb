# 4. Effects, failure and cancellation

## Effects

An **effect** is something a computation does besides producing a value. The
vocabulary is the twelve values of `sys.EffectKind`:

```text
table_read  table_write  network  filesystem  process  clock
randomness  secret_access  ui  git  storage_admin  runtime_admin
```

Effects are **inferred**. A function has no effect syntax: the checker computes
its effect set, publishes it in the function's signature metadata and shows it
in tools. The set takes no part in type compatibility: a function with effects
and one without have the same function type.

Only a body-less `native fn` states effects, in its declaration's effect clause
(chapter 1, "Native declarations"). For a native bound to a shared library that
clause is the author's claim, which the engine cannot check
(`docs/design/native-libraries.md`, section 5). Everything else derives them.

### Computing an effect set

Write `G |- e : T ! E` for "`e` has type `T` and effect set `E`".

```text
literal, variable, field read                  ! {}
lambda  (x => e)                               ! {}        (its body's effects belong to its calls)
e1 ; e2     e1 op e2    (e1, e2)   f(e1..en)   ! union of the parts
if c {a} else {b}                              ! E(c) U E(a) U E(b)
case s { p: b, ... }                           ! E(s) U E(every guard and body)
for p in e { b }                               ! E(e) U E(b)
call of declared function g                    ! E(args) U effects(g)
call of native fn n                            ! E(args) U declared(n)
```

1. `effects(g)` of a declared function is the union over its body, computed to a
   least fixed point over mutually recursive functions.
2. Creating a lambda has no effects. Calling it has the effects of its body.
   A local variable that holds a lambda keeps that lambda's effect set, so
   `let f = () => Room.delete(k); f()` has `table_write`.
3. A function that calls one of its function-typed parameters has, at each call
   site, the effects of the function actually passed. `xs | map(f)` has the
   effects of `f` plus those of `map`.
4. A call through a function value whose origin the checker cannot follow (an
   element of a list, a record field, a map value) has an unknown effect set.
   It counts as all twelve kinds. Every legality context below rejects it.
5. Table reads (`Room`, `Room | filter(...)`) are `table_read`; `insert`,
   `update`, `upsert`, `delete` and `rekey` are `table_write`; `now()` returns
   the activation time captured at start and has no effect; `uuid7()` is
   `randomness` and `clock`; `sleep` is `clock`; HTTP calls are `network`;
   file and process calls are `filesystem` and `process`.
6. Effects of an expression that is never evaluated because of short-circuit
   are still part of its set: effects are static.

### Where effects are restricted

A **context** limits which kinds a piece of code may have. The check is at the
use site against the inferred set.

| Context | Allowed | Everything else |
|---|---|---|
| Table assertion; module assertion; refined-type assertion | `table_read` of the candidate state | error |
| Computed field (`=>`) | none; no table access of any kind | error |
| Query callback (a function given to a relation operator such as `filter`, `map`, `sort_by`, `group_by`) | `table_read` | error |
| Read-only watch (a predicate or expression a watch evaluates) | `table_read` | error |

```orna
pub table Order(id: Uuid = uuid7()) {
    total: Decimal,
    tax: Decimal => total * 0.2,              // ok: no effect
    stamp: Instant => now(),                  // ok: captured time, no effect
    code: Str => fetch_code(total),           // error if fetch_code has network

    assert every(o => o.total >= 0.decimal);              // ok
    assert every(o => audit(o.id) == "");                 // error if audit writes
}
```

Rules:

* A restricted context may call only code whose full inferred set fits the
  allowed list. The diagnostic names the context, the first forbidden kind found
  and the call chain that introduces it. For assertions the code is the 1.1
  code for a forbidden effect (`ORNA-A091-007`).
* A callback is checked at the position where it is passed: its own body and
  everything it calls must fit. Passing a named function checks that function's
  inferred set.
* Assertions and computed fields are also deterministic for a fixed snapshot.
  That follows from the allowed list: no clock, randomness, network or secret
  access is possible in them.
* An effect-restricted context that needs a nondeterministic value takes it as
  a stored field instead (a default fills it once at insert time).
* Historical execution (code evaluated against a past snapshot) rejects
  `network`, `filesystem`, `process`, `secret_access`, `ui`, `git`,
  `storage_admin`, `runtime_admin` and `table_write`.

## Failure

A computation completes in one of three ways:

1. **success** with a value of its static type;
2. **failure** with an `Error` value;
3. **cancellation**.

Failure is not part of a value's type. A function declared to return `Int`
either produces an `Int` or fails; there is no `Result` and no `?` operator.
The checker may track which failures an expression can produce, but that is
tooling metadata: it is shown by tools, it does not affect type checking and it
is not part of the language rules.

An `Error` carries a `code: Str`, a `message: Str` and an optional `cause`:

```orna
let e = error(code: "email.invalid", message: "empty address");
fail(e)
```

`fail(e)` completes with failure `e`. Its type is the bottom type, so it can
stand wherever a value of any type is expected.

### Propagation

1. Evaluate subexpressions in the order of chapter 3.
2. If a subexpression succeeds, continue with its value.
3. If it fails and the enclosing construct is not the left side of `|?`, skip
   the rest of that construct and propagate the same failure outward through
   the expression, the function call and the activation. The error value, its
   cause chain and its source span are preserved.
4. At the activation boundary an unhandled failure rolls back the activation
   (below) and is reported to the caller.

### Recovery: `|?`

`e |? h` is typed and evaluated as:

```text
G |- e : T,  G |- h : fn(Error): T       =>  G |- (e |? h) : T
```

1. Evaluate `e`.
2. On success, the value is the result and `h` is not called.
3. On failure, call `h(error)` exactly once. Its result replaces the failed
   expression and evaluation continues after the `|?` stage; stages already
   completed are not run again.
4. If `h` fails, or calls `fail`, that failure propagates normally. `fail(err)`
   with the received `err` re-emits it unchanged.
5. The handler's result type must unify with `T`, so later stages see one type.
   A handler that always fails has the bottom type and fits any `T`.
6. A handler matches on the error's `code`; there is no typed catch syntax.
7. `|?` never receives cancellation (below).

```orna
fn port(raw: Str): Int =
    raw | Int.parse |? (_ => 8080);

fn port_strict(raw: Str): Int =
    raw | Int.parse |? (err => if err.code == "app.skip" { 8080 } else { fail(err) });
```

A longer pipeline reads left to right; the nearest `|?` that a failure reaches
handles it.

### Table writes and savepoints

Each table mutation (`insert`, `update`, `upsert`, `delete`, `rekey`) has a
savepoint. If the mutation itself fails, its partial changes are undone before
control reaches any `|?` handler, so a recovery handler never sees a
half-applied write. Writes made earlier in the activation stay in the pending
state; they commit or roll back with the activation.

```orna
fn drop_if_present(key: Uuid): Unit =
    Room.delete(key) |? (_ => ());
```

A missing key makes `delete` fail; its change set is empty, so the handler
resumes with `()` and the activation carries on. An assertion is different: it
is validated at the end of the activation (next section), so a false assertion
is not a failure of the `insert` and `|?` on the insert cannot recover it.

## Activations and rollback

An activation captures one snapshot of the database, one activation time and
opens one transaction.

1. All reads of the activation see the captured snapshot plus the activation's
   own pending writes.
2. Ordinary function calls inside the activation share its transaction. A
   helper never commits on its own.
3. Before the writes become visible, every assertion that the pending writes
   can affect is evaluated against the candidate state, in source order for
   one table, then module assertions after table assertions. A false assertion
   fails the activation with an assertion error naming a witness where one
   exists.
4. If the activation succeeds, every write commits together.
5. If a failure or cancellation escapes the activation (an unhandled failure,
   a false assertion, cancellation), every write the activation made is rolled
   back and nothing it wrote is visible to anyone.
6. A stream program does not hold one transaction for its lifetime: each item or
   configured batch is its own activation.

Work that left the engine is not undone: if a function sends a request and then
fails, the database changes roll back and the request has still been sent. The
tools say so rather than implying reversal.

A child activation (a `parallel` child or `sys.start` invocation) has its own
transaction. A child that committed before a sibling failed stays committed.
Starting a child from a parent that later fails or ends does not leave the
child running: ending the owner cancels and joins every unfinished child first.

## Cancellation

Cancellation is a completion state of its own. It is not an `Error`.

1. `|?` never handles cancellation, so a broad recovery handler cannot swallow
   it. `case` does not see it.
2. Cancellation is requested when the owner of the work (an activation or a
   session) ends, when a sibling fails in `parallel` or `race`, or when a
   timeout expires.
3. The runtime stops the work at the next cancellation point: a loop iteration,
   a call to the engine, a wait. Code between two points is not interrupted.
4. A cancelled activation rolls back its open transaction. Children that
   already committed stay committed.
5. Scoped cleanup (a function that guards a resource and releases it) runs on
   success, failure and cancellation, and is told which of the three occurred.
   The completion it observes for cancellation is "cancelled", never an
   `Error`.
6. A cancelled child cannot commit after the runtime has recorded its
   cancellation.

## Changes from 1.1.0

* Effects are inferred and published; legality is checked at the use site
  against the inferred set. 1.1 said effect annotations are not required but
  gave no computation rule.
* A call through a function value that cannot be traced counts as all effects.
* The restricted-context table is stated as allowed sets. Query callbacks and
  read-only watches are now restricted to `table_read`; computed fields to the
  empty set. 1.1 listed restrictions in prose per feature.
* Failure sets are tooling metadata only. `std.result` is deleted; `Result`,
  `Ok` and `Err` are not core.
* Cancellation is its own completion and is never caught by `|?`. In 1.1 the
  rule existed but was not tied to the recovery typing rule.
* Statement savepoints are stated as part of the evaluation rule, not as a
  library note.
* The postfix `?` propagation operator remains invalid; there is no `Result`
  unwrapping.

## Decided by

D2 (inferred effects, vocabulary, legality at the use site, no effect syntax),
D4 (unchecked failures, typed `Error`, rollback at the activation boundary,
cancellation as its own completion, `std.result` deleted), D10 (snapshot capture
by concurrent bodies; chapter 3), D12 (native declarations state their effects;
amended 2026-11-10: any module may declare one).
