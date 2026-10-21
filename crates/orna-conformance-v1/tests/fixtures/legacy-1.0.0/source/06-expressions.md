# 6. Declarations, expressions and failure {#expressions}

## Top-level declarations

Module units may declare imports, tables, functions, enums, protocols, dimensions, units, aliases, nominal/refined types, and constrained module assertions. Protocol implementations are nested in the nominal target type; currencies are ordinary types; there is no top-level `impl ... for ...` or `currency` declaration.

**ORNA-MODULE-001** Arbitrary expression statements MUST NOT appear at module top level.

**ORNA-MODULE-002** A module-scope `assert` is permitted only as the cross-table invariant form specified in [the tables chapter](#tables). It is not arbitrary module-load execution.

**ORNA-MODULE-003** Loading source parses, resolves, infers and type-checks declarations and assertion plans; it never starts streams, performs external work or mutates current table state.

Invalid:

```orna
http.get("https://example.com");
```

Valid cross-table declaration:

```orna
assert every(Invoice, invoice =>
    exists(Customer, customer => customer.id == invoice.customer_id)
);
```

## Functions and function values

Expression body with inferred return type:

```orna
pub fn square(x: Int) = x * x;
```

Explicit return annotation uses `:`:

```orna
pub fn square(x: Int): Int = x * x;
```

Block body:

```orna
pub fn create_note(text: Str): Note {
    let note = Note.insert({
        created: now(),
        text: text,
    });

    note
}
```

Defaults:

```orna
pub fn recent(rows, duration = 7.days) =
    rows | filter(row => row.time >= now() - duration);
```

**ORNA-FN-001** Calling a function MUST evaluate it at the time of the call against supplied values and the current database context.

**ORNA-FN-002** A qualified function name without call parentheses denotes the function value and MUST NOT invoke it.

```text
messages.sync      // function value
messages.sync()    // invocation
```

**ORNA-FN-003** Merely naming a function in the REPL MUST inspect the function and MUST NOT call it.

**ORNA-FN-004** A function declaration MUST NOT imply persisted materialization of its result.

**ORNA-FN-005** A function value has a statically known inferred or annotated parameter/return signature and may be stored in a local value, passed as an argument or returned from another function.

**ORNA-FN-006** An explicit return annotation follows the parameter list as `: Type`.

**ORNA-FN-007** `-> Type` is not function-return syntax in 1.0 and receives `ORNA091-E-RETURN-ARROW` with a mechanical `:` rewrite.

**ORNA-FN-008** Parameters and return types MAY omit annotations where inference succeeds.

**ORNA-FN-009** Every reachable return path and block tail contributes to return-type inference.

**ORNA-FN-010** A function's inferred failure set is metadata on the callable and is not wrapped into its successful return type.

**ORNA-FN-011** Merely declaring a function does not execute its body or its assertions.

**ORNA-FN-012** Public functions SHOULD use explicit boundary annotations when those improve API stability, documentation or diagnostics; this is guidance, not a requirement to annotate ordinary code.

## Enums and `case`

```orna
pub enum SyncResult {
    ok { count: Int },
    failed { reason: Str, retryable: Bool },
}

pub fn count(result: SyncResult): Int =
    case result {
        SyncResult.ok { count }: count,
        SyncResult.failed { reason: _, retryable: _ }: 0,
    };
```

**ORNA-ENUM-001** An enum is nominal and closed to variants declared in its body.

**ORNA-ENUM-002** A payload-free variant is referenced as `Enum.variant`.

**ORNA-ENUM-003** A payload variant uses a record payload and is constructed with explicit named fields, for example `SyncResult.ok { count: 3 }`. Destructuring uses record-pattern semantics, including the pattern-only shorthand `SyncResult.ok { count }`; construction does not permit that shorthand.

**ORNA-ENUM-004** Variant payload field order is source-preserving for presentation and structural for matching.

**ORNA-ENUM-005** A payload-free enum MAY be a primary-key component and is encoded by its canonical variant name. An enum variant carrying a payload MUST NOT be a primary-key component in version 1.0.

**ORNA-ENUM-006** Variant identifiers SHOULD use lowercase `snake_case` for readability, but identifier case remains part of the name.

**ORNA-CASE-001** `case` is an expression and MUST be exhaustive for a statically closed scrutinee type.

**ORNA-CASE-002** Each arm has `pattern [if guard]: body` form. The colon introduces the arm body; `=>` remains anonymous-function syntax.

**ORNA-CASE-003** Arms are considered in source order; the first matching pattern whose guard evaluates to true is selected.

**ORNA-CASE-004** All reachable value-producing arm bodies MUST type-unify.

**ORNA-CASE-005** A guard is evaluated only after its pattern matches and before its body.

**ORNA-CASE-006** The `match` keyword and `pattern => body` arm surface are invalid in 1.0 and receive `ORNA091-E-MATCH`.

**ORNA-CASE-007** A processor MUST NOT interpret `case` as exception handling. It is ordinary exhaustive value branching; failures are handled by `|?`.

## Generics, protocols and nested implementations

The protocol system is deliberately small and static. Bounds read like “`T` implements these protocols”:

```orna
pub protocol Display {
    fn display(self, context: DisplayContext): Str;
}

pub protocol Currency {
    static code: Str;
    static minor_digits: Int;
}

pub fn maximum<T impl Order>(values: [T]): T? =
    values | sort_by(value => value) | last();
```

Implementations live inside their nominal target:

```orna
pub type EmailAddress {
    value: Str,

    impl Display {
        fn display(self, context) = self.value;
    }
}
```

**ORNA-GENERIC-001** Generic parameters are explicit in public source when a generic abstraction is intended; local type arguments may be inferred.

**ORNA-GENERIC-002** Protocol dispatch is static in version 1.0. Overlapping implementations are invalid.

**ORNA-GENERIC-003** A protocol implementation is lexically nested in the nominal target type. The current database must own that type declaration.

**ORNA-GENERIC-004** Specialization, associated types, higher-kinded types, negative implementations and runtime protocol objects are outside version 1.0.

**ORNA-GENERIC-005** A generic protocol bound uses `<T impl Protocol>`; several bounds use `+`, for example `<T impl Display + Order>`.

**ORNA-GENERIC-006** The removed colon-bound spelling `<T: Protocol>` is invalid and receives `ORNA091-E-BOUND-COLON`.

**ORNA-GENERIC-007** The removed top-level form `impl Protocol for Type` is invalid and receives `ORNA091-E-IMPL-FOR`; the implementation is moved into `Type` and omits `for Type`.

**ORNA-GENERIC-008** A protocol may declare instance functions and static properties.

**ORNA-GENERIC-009** A static protocol property is declared `static name: Type;` and implemented `static name = expression;`.

**ORNA-GENERIC-010** `static fn` is not a protocol-member category in 1.0. Behavior that does not require an instance is expressed as an ordinary function or as construction/conversion owned by the target type.

**ORNA-GENERIC-011** Every required protocol member must have exactly one compatible implementation in a conforming nested `impl` block.

**ORNA-GENERIC-012** Nested implementations may access the target type's private representation.

**ORNA-GENERIC-013** A nominal target MAY contain several nested implementations, including several different instantiations of one generic protocol such as `From<Str>` and `From<EmailHeader>`.

**ORNA-GENERIC-014** Implementation selection MUST NOT depend on source order when the target/source/protocol types are otherwise identical; such overlap is a static error.

**ORNA-GENERIC-015** A table declaration MAY contain nested protocol implementations for its nominal row type; those implementations are not stored row fields and do not affect table identity or storage.

## Control flow and local bindings

Orna uses braces to delimit blocks. Conditional expressions produce values, and local bindings may be reassigned as shown below.

```orna
let label =
    if total > 100.GBP {
        "large"
    } else {
        "small"
    };

let total = 0;
for item in items {
    total = total + item;
}
```

**ORNA-CFLOW-001** A block evaluates from left to right. A final expression without `;` is the block value; otherwise the block value is `Unit`.

**ORNA-CFLOW-002** Function receivers/arguments, list elements and record field expressions evaluate left to right in source order. `&&` and `||` short-circuit left to right.

**ORNA-CFLOW-003** `if` is an expression and all value-producing branches MUST type-unify.

**ORNA-CFLOW-004** `case` is an expression and MUST be exhaustive for closed types.

**ORNA-CFLOW-005** `let` creates a function-local binding slot containing an immutable value. Assignment may replace that slot's value when the pattern denotes one local identifier.

**ORNA-CFLOW-006** Assignment is a statement targeting an existing local `let` binding. It is not an expression and cannot occur inside a record field, argument, condition or returned value.

```orna
let count = 0;
count = count + 1;
count += 1;
```

**ORNA-CFLOW-007** `for` consumes a finite `Iterable`. An unbounded `Stream` is consumed by stream operations such as `for_each`.

**ORNA-CFLOW-008** `return` exits the current function; `break` exits the nearest loop and may carry a value; `continue` advances the nearest loop.

**ORNA-CFLOW-009** Orna has no implicit truthiness.

**ORNA-CFLOW-010** A braced control-flow expression (`if`, `case`, `for`, `while`, or `loop`) MAY be used as a statement without a trailing semicolon. When it is the final item of a block, it is the block's tail expression unless followed by `;`.

**ORNA-PARSE-002** While parsing a block, a braced control-flow expression followed by another block item is a statement; the final unsemicolonated expression immediately before `}` is the block tail. `loop` used as a non-final statement requires `;` when its value would otherwise be ambiguous.

**ORNA-CFLOW-011** The removed `var` declaration MUST NOT be treated as a second mutability system. The mechanical migration is `var name = value;` to `let name = value;`.

## Entry functions and `orna run`

**ORNA-RUN-001** `orna run <qualified-function>` MUST invoke the named public function.

**ORNA-RUN-002** `orna run` without a function name MUST invoke root `main()` if present and MUST otherwise produce a diagnostic.

**ORNA-RUN-003** A file path MUST NOT acquire special execution behavior merely because it is named `ingest/main.orna` or appears under a particular directory.

A finite program exits when complete. A program consuming an unbounded stream remains active until cancelled or the source closes.

## Anonymous functions and closures

Anonymous functions use `=>`:

```text
value => value.name
(a, b) => a + b
() => sync(account)
message => {
    let decoded = decode(message);
    Email.insert(decoded);
}
```

**ORNA-LAMBDA-001** One unparenthesized identifier or `_` before `=>` is a one-parameter anonymous function.

**ORNA-LAMBDA-002** Zero or multiple parameters use parentheses.

**ORNA-PARSE-001** When `(` begins an expression, the parser scans to the matching `)` while respecting nested delimiters. If the next significant token is `=>`, the construct is an anonymous-function parameter list; otherwise an empty pair is Unit, a comma at the outermost parenthesis level denotes a tuple, and a nonempty comma-free pair groups one expression. Implementations may use equivalent parsing machinery, but MUST produce the same parse.

**ORNA-LAMBDA-003** Anonymous functions capture immutable lexical values. Such a value is called a closure when it captures at least one surrounding value.

**ORNA-LAMBDA-004** `|...|` is not anonymous-function syntax. `|` is successful-value pipeline application, `|?` is failure recovery and `||` is logical OR.

**ORNA-LAMBDA-005** Anonymous-function direct braced bodies follow [the lexical chapter](#lexical).

A bare named function is already a function value:

```orna
parallel([
    messages.sync,
    ledger.sync,
])
```

Use `() => ...` only when inline work or captured arguments are actually required.

## Operators, success pipelines and recovery pipelines

From lowest binding power to highest:

| Level | Forms | Associativity |
|---:|---|---|
| 1 | anonymous function `=>` | right |
| 2 | logical OR `\|\|` | left, short-circuiting |
| 3 | logical AND `&&` | left, short-circuiting |
| 4 | comparison `==`, `!=`, `<`, `<=`, `>`, `>=`, `in` | one operator; no chaining |
| 5 | option coalescing `??` | right |
| 6 | pipelines `\|`, `\|?` | left |
| 7 | ranges `..`, `..=` | non-associative |
| 8 | addition/subtraction `+`, `-` | left |
| 9 | multiplication/division/remainder `*`, `/`, `%` | left |
| 10 | exponent `^` | right |
| 11 | unary `!`, `-`, `+` | right |
| 12 | field access, calls and indexing | left |

**ORNA-OP-001** A conforming parser MUST apply the precedence and associativity table above. Parentheses override it.

**ORNA-PIPE-001** `|` is the canonical successful-value pipeline operator and has no bitwise meaning.

**ORNA-PIPE-002** `value | function` is exactly `function(value)` after `value` completes successfully.

**ORNA-PIPE-003** `value | function(a, named: b)` is exactly `function(value, a, named: b)`. The piped value is inserted as argument one.

**ORNA-PIPE-004** Any callable may be a success-pipeline stage when its first parameter accepts the piped value. There is no separate pipe-function declaration or category.

**ORNA-PIPE-005** A direct anonymous function used as a pipeline stage MUST be parenthesized:

```text
rows | (row => row.name)      // valid
rows | row => row.name        // invalid
```

**ORNA-PIPE-006** Bitwise operations are complete ordinary functions supplied by `std.bits`, such as `bit_or`, `bit_and`, `bit_xor`, `bit_not`, `shift_left` and `shift_right`.

**ORNA-PIPE-007** If evaluation to the left of `|` fails, the stage to its right is not evaluated and the failure continues outward.

**ORNA-PIPE-008** `expression |? handler` invokes `handler(error)` only when `expression` fails. On success, the original value passes through without invoking the handler.

**ORNA-PIPE-009** A recovery handler's successful return type MUST unify with the successful type to its left so later pipeline stages receive one static type.

**ORNA-PIPE-010** If a recovery handler fails or explicitly calls `fail(error)`, that failure propagates normally.

**ORNA-PIPE-011** `|` and `|?` associate leftward, permitting readable success/recovery sequences:

```orna
raw
    | decode
    | validate
    |? recover_invalid_message
    | store
```

**ORNA-PIPE-012** `|?` is ordinary explicit recovery, not assertion syntax and not a stream dead-letter operation.

**ORNA-COALESCE-001** `a ?? b` evaluates `a` first. If `a` is `Some(value)`, it yields that value without evaluating `b`; otherwise it evaluates and yields `b`.

**ORNA-COALESCE-002** `??` associates rightward: `a ?? b ?? c` means `a ?? (b ?? c)`.

**ORNA-COMPARE-001** Equality and comparison operators do not chain. `a < b < c` and `a == b == c` are syntax errors; source writes `a < b && b < c` when that is intended.

**ORNA-COMPARE-002** Comparison operands evaluate left-to-right and each accepted operator yields `Bool`.

## Fields, selectors and member calls

Field/property access, declared member calls and pipeline application are distinct.

```text
contact.full_name                 // stored/computed field selector
Contact.full_name(contact)        // the same selector as a first-class function
contact | transactions            // ordinary free function application
relation.count()                  // an actual declared Relation member
GBP.code                          // static protocol property
```

**ORNA-MEMBER-001** `value.field` resolves only a field selector declared by the value's nominal/structural type.

**ORNA-MEMBER-002** Every table field generates a first-class selector named `Table.field`. Selecting `row.field` applies that selector to the row.

**ORNA-MEMBER-003** `value.member(args)` is valid only for a member or protocol operation explicitly associated with the receiver type. Importing a free function whose first parameter happens to match MUST NOT silently make it a dot method.

**ORNA-MEMBER-004** Free functions remain ordinary calls and may always be used through the pipeline when argument one is compatible.

**ORNA-MEMBER-005** Core types may define genuine associated members such as relation `count`, `first`, or codec `encode`; this does not create unrestricted UFCS.

**ORNA-MEMBER-006** `Type.static_property` resolves a static property supplied by a protocol implementation and does not require construction of a value.

## Failure propagation and recovery

Ordinary source does not unwrap successful values. A called operation either produces its successful type or abruptly fails:

```orna
let decoded = std.encoding.json.decode(raw, as: Message);
let stored = Message.insert(decoded);
```

If decode fails, insertion is not attempted; the original failure leaves the function and, if unhandled, the activation.

```orna
let decoded =
    std.encoding.json.decode(raw, as: Message)
    |? (failure => Message.default());
```

**ORNA-ERR-001** A failure MUST propagate automatically through the current expression, function and activation until a matching `|?` recovery boundary or host boundary handles it.

**ORNA-ERR-002** Automatic propagation MUST preserve the original error value, causal chain and source-span information where available.

**ORNA-ERR-003** `fail(error_value)` abruptly completes with that error and has the bottom successful type, allowing it where any successful type is expected.

**ORNA-ERR-004** Cancellation is a distinct abrupt completion, not a normal `Error` that a broad `|?` handler may accidentally swallow.

**ORNA-ERR-005** Activation-owned resources MUST be released on return, failure, panic or cancellation. Deterministic cleanup SHOULD use scoped functions rather than user finalizers.

**ORNA-ERR-006** An unhandled failure in a database-writing activation MUST abort and roll back every Orna-controlled write in that activation.

**ORNA-ERR-007** External effects already performed before failure are not transactionally reversible; diagnostics and retry policy MUST remain honest about that boundary.

**ORNA-ERR-008** A postfix propagation operator `?` is invalid in expression position and receives `ORNA091-E-POSTFIX-QUESTION`.

**ORNA-ERR-009** There is no intrinsic `Result<T,E>`, `Ok(...)` or `Err(...)` failure surface. An unresolved use in that role receives `ORNA091-E-RESULT`. Independently declared user types and variants with these names remain ordinary declarations; this rule does not reserve their names.

**ORNA-ERR-010** Recovery uses `|?`, ordinary functions, `case` over ordinary values, or a combination of them. It does not require a parallel exception syntax.

**ORNA-ERR-011** A recovery handler executes at most once for the failure occurrence presented to that `|?` stage.

**ORNA-ERR-012** A successful recovery resumes evaluation at the next enclosing expression/pipeline point; it does not restart already completed stages.

### Algorithm FAILURE-1: expression propagation

1. Evaluate subexpressions in the order required by the containing construct.
2. If a subexpression completes successfully, continue with that value.
3. If it fails and the immediately enclosing construct is not the handling side of `|?`, skip remaining work in that construct and propagate the same failure.
4. If it is the left side of `|?`, invoke the handler exactly once with the failure value.
5. If the handler succeeds, substitute its value and continue. If it fails, propagate the handler's failure.
6. At an activation boundary, roll back Orna-controlled writes before reporting an unhandled failure.



## Completion states

Every expression evaluation has one of three completion states:

1. **success** with the expression's static value type;
2. **failure** with an `Error` value;
3. **cancellation**, which is controlled by runtime structured-concurrency semantics.

Failures are not successful values and are not encoded through an implicit enum wrapper. Optimizers and backends may use tagged internal representations, but source, type display, `sys.Function.return_type`, equality and codecs observe only the specified model.

**ORNA-FAILURE-001** A successful static return type excludes the failure channel.

**ORNA-FAILURE-002** The compiler MUST track whether reachable operations may fail sufficiently to preserve control flow, rollback and diagnostics, without requiring source-visible effect annotations in 1.0.

**ORNA-FAILURE-003** Public metadata SHOULD expose the inferred set of nominal failure types where knowable and `Error` where open-ended.

**ORNA-FAILURE-004** A broad recovery handler receives at least `Error`; a processor MAY infer a narrower nominal error type from the left expression.

**ORNA-FAILURE-005** Error values are ordinary inspectable values when explicitly received by a recovery handler, but they do not become successful results merely because they are values there.

**ORNA-FAILURE-006** Calling `fail(e)` from a recovery handler re-emits `e` unless it constructs another error deliberately.

**ORNA-FAILURE-007** Nested `|?` stages handle the nearest failure that reaches them according to left-associative pipeline evaluation.

**ORNA-FAILURE-008** A handler that returns a fallback value resumes with that value and does not rerun earlier successful/effectful stages.

**ORNA-FAILURE-009** Host boundaries record unhandled errors in `sys.Run`/`sys.Failure` as applicable and present a safe diagnostic.

**ORNA-FAILURE-010** The runtime MUST distinguish an activation failure from a preserved stream-item failure record; the latter is durable administration data produced by checkpoint policy.

## Recovery examples

```orna
fn parse_port(raw: Str): Port =
    raw
        | Int.parse
        | Port.from
        |? (failure => {
            log.warn(failure);
            8080 | Port.from
        });
```

```orna
fn require_message(raw: Str): Message =
    raw
        | std.encoding.json.decode(as: Message)
        |? (failure => fail(error(
            code: "message.invalid",
            message: "could not decode message",
            cause: failure,
        )));
```

