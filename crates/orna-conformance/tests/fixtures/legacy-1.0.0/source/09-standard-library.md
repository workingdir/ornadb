# 9. Core operations and the standard library {#standard-library}

The language supplies a small intrinsic environment sufficient to evaluate source, query and update tables, consume streams, validate assertions and inspect `sys`. The optional `std` namespace contains ordinary, explicitly imported library code pinned to a Git snapshot. The two are not interchangeable: deleting an optional formatter or connector cannot disable database integrity.

## Resolution and versioning

Core type names, `Some`, `null`, `error`, `fail`, `now`, `uuid7` and the relational/stream operations below are available in the root intrinsic environment. Ordinary lexical declarations may shadow unqualified helpers; `sys` itself cannot be shadowed. `CWD` and `HEAD` are context-bound snapshot selectors, not wall-clock values. Evaluating `HEAD` in an unborn repository fails; it does not produce an empty synthetic commit.

`std` imports use ordinary `use` rules. `use std.prelude as _;` imports the prelude exports recorded by that pinned module; it does not import every standard module. `std.query` and `std.stream` may re-export the intrinsic operators with identical identities and behaviour. A user-defined function of the same name remains an ordinary function and is checked for the effects required by its use.

**ORNA-LIB-001** A program's library behaviour MUST be determined by its captured dependency snapshots. A historical program cannot silently substitute the currently installed `std` or current time-zone data.

**ORNA-LIB-002** Core integrity, transactions and assertions MUST work without `std`. An absent optional module produces the ordinary import diagnostic; it cannot be filled by an undocumented host library with the same spelling.

**ORNA-LIB-003** This section's portable operations MUST have the stated argument, order, failure and effect semantics when supplied under the reference-library profile. Other modules expose their exact public declarations through their pinned source and `sys`; their mere namespace name is not a claim of an unspecified portable API.

## Signature notation

In the following tables, `T`, `U` and `K` are type parameters, and `fn(T): U` is a function type. `Collection<T>` in explanatory prose means either `[T]` or `Relation<T>`; it is not a new source type. Each listed collection operation has those two explicit overloads and preserves the input container kind unless its result is scalar. `Stream<T>` operations are separate because they have waiting, ownership and checkpoint effects.

Arguments are evaluated left to right. A pipeline supplies argument one: `rows | filter(test)` calls `filter(rows, test)`. It does not first call `filter(test)` and guess a missing collection. Predicate factories explicitly listed below are genuine one-argument calls.

## Core relation operations

| Operation | Successful result | Contract |
|---|---|---|
| `filter(rows, predicate: fn(T): Bool)` | Same collection kind of `T` | Retain matching values in input order. No implicit truthiness. |
| `map(rows, transform: fn(T): U)` | Same collection kind of `U` | One result per input, preserving order. |
| `flat_map(rows, transform)` | Same collection kind | Concatenate each finite returned collection in input order; preserve inner order. |
| `sort_by(rows, key: fn(T): K)` | Same collection kind of `T` | Stable ascending order by K; ties preserve input order. Float uses total order. |
| `take(rows, count: Int)` / `drop(rows, count: Int)` | Same collection kind | Nonnegative count; do not enumerate beyond what the result requires. Negative count fails. |
| `distinct(rows)` | Same collection kind of `T` | Keep the first value in each lawful equality class. Default Float hashing is unavailable. |
| `union(left, right)` | Common collection kind | Concatenate left then right; duplicates remain. Use `distinct` explicitly for set-like behaviour. |
| `count(rows)` | `Int` | Exact finite count. Resource failure is explicit, not a truncated count. |
| `first(rows)` | `T?` | `Some(first value)` or `null`; need not enumerate the remainder. |
| `one(rows)` / `one(rows, predicate)` | `T` | Require exactly one matching value; zero and multiple matches are distinct cardinality errors. |
| `sum(rows)` | Numeric element type | Exact addition for Int/Decimal/Money; additive zero for empty input; Float follows observable order. |
| `min(rows)` / `max(rows)` | `T?` | `null` for empty input; otherwise `Some`. Float follows the specified NaN/signed-zero rules. |
| `every(rows, predicate)` | `Bool` | True for empty input; short-circuit at the first false value in observable order. |
| `exists(rows, predicate)` | `Bool` | False for empty input; short-circuit at the first true value. |

Relation members `count`, `first`, `one`, `as_of` and generated field selectors are real associated operations, not unrestricted free-function-to-method conversion. Other operators use explicit calls or pipelines unless the type declares an associated member.

**ORNA-LIB-004** Read-only query callbacks MUST be effect-checked before optimisation. A relational plan may not hide an external side effect inside an expression that an optimiser could reorder, omit or repeat.

**ORNA-LIB-005** Failed callbacks propagate through the ordinary failure channel. A collection operator MUST NOT skip failed elements or substitute `null` unless that behaviour is requested by a separately named operation.

### Predicate factories

`Predicate<S>` is the structural callable type `fn(S): Bool`. `every(test)` constructs a predicate over `Relation<T>` that applies the row test. `all_unique(selector)` constructs a predicate over `Relation<T>` whose selected keys must have a lawful equality relation and compatible hash or order implementation. No `Float`-based default equality key is accepted.

`all_unique` compares complete selected values. For optional selected keys, `null` equals `null`; two absent values therefore violate uniqueness. To exclude missing values, write an explicit deterministic predicate over a filtered relation. There is no implicit SQL-style null exclusion. On failure, the reference witness is the earliest duplicate pair in canonical row order. An index implementation must produce that same safe witness when one is requested.

```orna
pub table Account(id: Str) {
    handle: Str,
    assert all_unique(account => account.handle);
}
```

Factories are ordinary functions, not additional keywords. The declaration supplies the candidate relation; the source does not repeat it through `self |`.

## Core table operations

Table mutation arguments are checked against a record shape derived from the table schema. This shape is a compile-time constraint and has no separately exported source type.

| Call | Result | Required behaviour |
|---|---|---|
| `T.insert(fields)` | `T` row value | Construct a complete new row, evaluate defaults once and fail on an existing key. Missing automatic/defaulted keys are allocated; missing other keys fail. |
| `T.upsert(fields)` | `T` row value | Insert when absent; otherwise replace explicitly supplied stored non-key fields while preserving omitted existing fields. On the insert path all required fields must be supplied or defaulted. |
| `T.update(key, fields)` | `T` row value | Require an existing row; patch only supplied stored non-key fields. Unknown, computed and primary-key fields fail before mutation. |
| `T.delete(key)` | `Unit` | Require an existing row and delete it from the candidate state. References are checked at commit. A missing key is an error, not a successful deletion count. |
| `T.rekey(old_key, new_key)` | `T` row value | Require an explicitly keyed table; preserve fields and semantic change identity, then validate references at commit. |

A one-component key is supplied as that component's type. A multi-component key is a tuple in declaration order. Each operation observes earlier successful writes in the same activation, but returned row values remain immutable observations. No mutation directly edits an already held row value.

**ORNA-LIB-006** Mutation return values and missing-key behaviour MUST follow this table. Errors include the table identity and safe key, without disclosing redacted values. A failing operation's tentative writes cannot leak into the rest of the activation through a recovery handler.

For the last rule, each mutation has a statement savepoint: a recoverable operation failure restores its own partial changes before control reaches `|?`. Earlier successful operations in the enclosing activation remain tentative until that activation commits or rolls back. A declaration assertion is validated at the enclosing transaction boundary and may still abort the entire activation.

## Core finite streams

`Stream.from_list(values, source_identity: Str)` constructs a finite, replayable list-backed stream for deterministic input and testing. It performs no I/O. The effective source identity combines the supplied name with the canonical typed digest of the complete immutable list. Reusing a label for different list contents therefore cannot resume against the old contents by accident.

The sole partition is `null`. Position format `orna.list.v1` stores the zero-based index of the next item as an unsigned canonical integer. Position 0 precedes the first item; position equal to the list length denotes exhaustion. Resume outside 0…length fails. The successor of item i is i+1. A provider cannot treat the index as a mutable list offset after construction.

`for_each(stream, action: fn(T): Unit)` processes one item at a time. Each callback and corresponding checkpoint movement is a separate activation, and the enclosing call returns `Unit` after exhaustion. A returned value other than Unit must be discarded explicitly in the callback body. The operator propagates cancellation and reports blocked failures through the checkpoint model rather than silently continuing.

**ORNA-LIB-007** The list-backed source MUST supply the complete declared identity, replay and preservation contract. It MUST NOT require an unprovided connector host or external account.

## Collection and query library

The following helpers are supplied by `std.collection` and `std.query`. They are pure for pure inputs/callbacks; their finite-input and ordering requirements are checked rather than hidden behind unbounded buffering.

| Function | Contract |
|---|---|
| `chunk(values, size)` | Consecutive nonempty chunks; final chunk may be shorter. Size must be positive. Empty input gives no chunks. |
| `flatten(values)` | One level of concatenation, preserving outer and inner order. |
| `partition(values, predicate)` | A two-element tuple `(matching, remaining)`, each preserving input order. Predicate evaluated once per value. |
| `zip(left, right)` | Ordered pairs until the shorter collection ends. `zip_exact` fails when lengths differ. |
| `unique(values)` | Same first-occurrence semantics as core `distinct`; this is a library helper, not a field modifier. |
| `group_by(values, key)` | Groups in key order, each group's rows in input order. Key comparison must be lawful. |
| `pairs(values)` | Adjacent overlapping pairs; length n produces max(0,n−1) pairs. |
| `window(values, size, step = 1)` | Complete positional windows only; positive size and step. A separately named partial-window option must be explicit. |
| `split_when(values, predicate)` | Start a new group before an item where the boundary predicate is true; no empty groups are inserted. |
| `rank(values, key)` | Stable sort by key, then assign one-based competition ranks; equal keys share a rank, and the next rank skips their count. |
| `asof_join(left, right, time, by)` | For each left row choose the latest right row at or before its time in the equal `by` group. Equal right timestamps resolve by right canonical key order, choosing the last. Missing matches are `null`. |
| `bucket_by(values, period, zone)` | Group time values by explicit elapsed or calendar boundaries. Calendar bucketing of Instant requires a zone. No fixed-seconds approximation of a local day is allowed. |

`pivot`, `unpivot` and domain-specific query helpers may be provided by a pinned library package. Their result schemas must be statically derivable or explicitly selected; they cannot introduce an implicit dynamic-record fallback. Their exact package declarations, not this catalogue label, determine their optional API.

## Standard stream operators

| Operation | Behaviour and limits |
|---|---|
| `batch(stream, size)` | Ordered batches of at most positive size; the final batch may be shorter. Checkpoint covers the whole committed batch. |
| `buffer(stream, capacity)` | Bounded queue with positive capacity; default backpressure, no implicit drop. |
| `merge(streams)` | Interleave by observed arrival order, preserving each input's order. Cross-source order is intentionally nondeterministic and cannot be used as a deterministic assertion. |
| `throttle(stream, duration)` | Emit at most one item per positive elapsed interval; the caller selects and records the explicit drop policy before a nonreplayable source is admitted. |
| `debounce(stream, duration)` | Retain the latest item until a positive quiet interval elapses; dropped intermediate delivery semantics must be declared, not mistaken for durable processing of every item. |
| `retry(stream, policy)` | Retry the same blocked delivery under its provider contract and bounded delay policy; never silently advance progress on exhaustion. |
| `recover(stream, handler)` | Explicitly handles source/decoding errors only where the provider contract permits recovery. It cannot acknowledge an unprocessed ordered delivery without the skip protocol. |

Stateful operators must either keep restart-recomputable state or persist it in ordinary tables together with the input checkpoint. A buffer in RAM is not a durable checkpoint. A pipeline combining several checkpointed source roots must separate them into named consumer functions with explicit durable identities.

## Concurrency, text and bit operations

`std.concurrent.parallel`, `race` and `timeout` follow [task ownership](#execution). `sleep(duration)` is cancellation-aware, accepts nonnegative elapsed duration and does not promise exact scheduling at the requested instant. It has a clock/waiting effect and is not permitted in declaration assertions.

`std.text` provides `trim`, `split`, `join`, `starts_with`, `ends_with`, `contains`, `replace`, `normalise`, `lower` and `upper`. Text indices and lengths are Unicode scalar positions unless the function is explicitly named for UTF-8 bytes or grapheme clusters. Casing and normalisation use the pinned Unicode data version; locale-sensitive operations require an explicit locale. `split` preserves empty fields, including trailing fields; an empty separator splits into scalars. Replace is non-overlapping left-to-right. Regex behaviour is part of an explicitly pinned regex package, not an implicit host dialect.

`std.bits` provides `bit_or`, `bit_and`, `bit_xor`, `bit_not`, `shift_left` and `shift_right` over Int using an unbounded signed two's-complement model. Shift counts must be nonnegative. Right shift is arithmetic; left shift is exact subject to resource limits. `bit_not(x) = -x - 1`. The source token `|` always remains a pipeline.

## Exact money, time and statistics

`Currency` is a core protocol because `Money<C>` must remain meaningful without `std`. `std.money` re-exports that protocol and supplies explicit rounding, allocation and formatting. Currency symbols and placement are locale data; `GBP.code` is identity metadata, not a universal display symbol.

`allocate(amount, weights)` requires nonnegative exact integer weights and a positive total weight. It rounds each share toward zero at the currency's minor-unit scale, then distributes remaining minor units by descending exact fractional remainder, ties by input index. Negative amounts allocate the magnitude and restore signs. The shares sum exactly to the original minor-unit amount; a non-minor-unit input requires an explicit rounding choice first.

`std.time` distinguishes elapsed arithmetic from calendar arithmetic. Ambiguous local times require an explicit earlier/later offset choice; nonexistent local times fail unless an explicit adjustment policy is supplied. The time-zone database edition is recorded with dependency/runtime metadata. Formatting uses a supplied immutable locale/time-zone context.

The `duration.compact`, `duration.clock`, `duration.words` and `duration.iso` formatter values expose `.format(value, context: ...)`. They are interchangeable presentation choices, not storage conversions. The ISO form is an elapsed duration format, not a way to turn a month into a fixed number of seconds.

`std.stats.mean` returns `null` for empty input. Exact inputs retain exact sums; a nonterminating exact division requires explicit scale/rounding. `median` uses total-order sorting and returns the middle value, or the explicitly rounded/exact mean of the middle two. `percentile` requires a probability in [0,1] and a named interpolation method; there is no unrecorded host default. Histogram bins are ordered, nonoverlapping half-open ranges except an explicitly closed final upper bound. Rate, derivative and integration require ordered timestamps and state their unit transformation and treatment of equal timestamps.

## Codecs and host I/O

Every codec owns `encode(value)` and `decode(input, as: T)`. Canonical Orna text and the binary value profile are defined in [canonical formats](#formats). JSON decoding is schema-directed, rejects duplicate object keys, distinguishes missing fields from explicit null, and parses numbers without first rounding through binary Float. Unknown fields fail unless an explicit decoding option permits ignoring them. JSON encoding of unsupported values fails rather than dropping fields or emitting nonstandard numeric tokens.

Base64 uses the RFC 4648 standard alphabet with `=` padding and no inserted whitespace. The decoder rejects invalid characters, noncanonical padding and unused nonzero trailing bits. URL-safe Base64 is a separately named codec profile. CSV, XML, YAML, TOML and MIME are optional codec packages; their source pins, schema mapping and selected format editions must be stated by the package before interoperability is claimed.

`std.io.fs`, `std.io.process` and `std.net` are explicit host-effect boundaries. A filesystem operation states its root/path and overwrite mode, process invocation separates executable from argument strings, and HTTP/WebSocket clients expose status, headers, body, cancellation and bounded reads. No shell command is assembled implicitly from interpolated strings. These operations are unavailable to assertions, read-only watches, presenters and historical execution. Their errors propagate normally; database rollback does not undo an external write or request.

SOPS is an optional secret provider. A stable `SecretRef` is not secret plaintext; resolving it is a separately checked host effect. Generic encoding and presentation never reveal the resolved value.

## Presentation helpers and tests

`std.ui` functions construct the core presentation tree. A helper such as `Field(label, value)` explicitly presents the typed value and returns a presentation node; it is not an implicit heterogeneous-list conversion. `Text`, `Rows`, `Cols`, `Stack`, `Details`, `Table`, `Tree`, `Code`, `Diff` and `Chart` all retain an Inspect-compatible fallback. `Button`, `Form` and `Input` carry typed action/input descriptions; only a server-created action handle can execute an event.

`std.test` supplies expectations, fixtures, property generators and isolated database helpers. Language-level `assert` remains core. A test fixture declares its source, initial snapshots, inputs, expected success/failure and external-effect policy. A temporary branch is not isolation unless the worktree-local CWD and runtime ownership are isolated too. No test may silently use the developer's local database, credentials or live services.

