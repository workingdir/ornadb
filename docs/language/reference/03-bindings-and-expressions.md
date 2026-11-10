# 3. Bindings, expressions and patterns

## Variables

`let` binds a variable. `x = value;` assigns to it later. There is one binding
form: no `var`, no `mut`.

```orna
let total = 0;
for item in items {
    total = total + item;
}
total += 1;
```

A variable is a **slot** that holds one immutable value. Assignment replaces the
value in the slot. It never changes a value that was stored, returned, captured
or read through another name.

Rules:

1. `let p = e;` evaluates `e`, matches it against the pattern `p` (which must
   always match) and creates one new slot per name in `p`. `let p: T = e;`
   checks `e` against `T`.
2. A `let` may reuse a name from an enclosing block or from earlier in the same
   block. It creates a new slot; the old slot stays valid for whoever captured
   it.
3. `x = e;` is a statement. `x` must be a variable introduced by a `let` whose
   pattern was a plain identifier, in the current function or captured from an
   enclosing function. `e` must have the variable's type: `x = e;` is
   well-typed when `x : T` and `e : T` (a `T` lifts to `T?` as usual).
   Parameters, pattern variables from `case`, `for` and destructuring `let`s
   are not assignable.
4. `x op= e;` for `+=`, `-=`, `*=`, `/=` means `x = x op e;`. `x` is evaluated
   once.
5. Assignment is not an expression. It cannot appear inside an argument, a
   record field, a condition or a block tail.
6. There is no implicit truthiness: a condition has type `Bool`.

### Closures share the slot

A lambda or nested function value captures the **variable**, not a copy of its
value at creation time. Within one activation:

```orna
let n = 0;
let bump = () => { n += 1; };
bump();
bump();
// n == 2

let get = () => n;
n = 10;
// get() == 10
```

* A closure sees every later assignment to a captured variable and its own
  assignments are seen by the enclosing code.
* Each execution of a `let` creates a new slot. A `let` inside a loop body makes
  a fresh slot per iteration, so closures created in different iterations
  capture different slots. A `for` pattern variable is fresh per iteration.
* A slot lives as long as any closure that captured it. Slots belong to the
  activation that created them and are never visible to another activation.
* Values stay immutable: a closure that captures a list variable sees whole
  list values come and go in the slot, never a list changed in place.

### Concurrent bodies capture snapshots

The bodies handed to the engine's concurrent combinators (`parallel`, `race`,
`timeout`) and to `sys.start` run as separate activations. They capture the
**current value** of each outer variable at the moment they are started; they
share no slot with the parent or each other.

```orna
let base = 10;
let results = parallel([
    () => base + 1,        // reads the value 10
    () => base + 2,
]);
base = 20;                  // does not change what the children saw
```

Rule: an assignment, inside a concurrent body, to a variable declared outside
that body is a compile error. A variable declared inside the body is local to
it and assignable, and the body may return a result for the parent to bind.

```orna
let count = 0;
parallel([() => { count += 1; }]);    // error: assigns to an outer variable
```

The rule is static, so concurrent children cannot race on a slot. A closure
created inside a concurrent body and assigning to a variable of the body is
allowed; one assigning to a variable outside the body is the same error.

## Blocks

`{ item; item; tail }` evaluates its items left to right. The value is the
final expression when there is no trailing `;`; otherwise `Unit`. A `let` is
scoped to the rest of its block.

A braced control expression (`if`, `case`, `for`, `while`, `loop`) can be a
statement without `;`. As the last item before `}` it is the block's value
unless a `;` follows. A free-standing block is not an expression; blocks occur
only as function, lambda, `case` arm and control bodies.

```orna
pub fn label(total: Decimal): Str {
    let size = if total > 100.decimal { "large" } else { "small" };
    "{size} order"
}
```

## Control flow

**`if`** is an expression. `if c { a } else { b }` has the joined type of `a` and
`b`. Without `else` the type is `Unit` and the body must have type `Unit`.
`else if` chains are nested `if` expressions.

**`case`** matches a value against arms `pattern [if guard]: body`.

```orna
case shape {
    Shape.circle { radius }: radius * radius * 3.decimal,
    Shape.rect { w, h } if w == h: w * w,
    Shape.rect { w, h }: w * h,
    Shape.empty: 0.decimal,
}
```

1. Arms are tried in source order. The first arm whose pattern matches and whose
   guard (if any) is true is selected; the guard is evaluated only after the
   pattern matched.
2. Arm bodies have one joined type; that is the type of the `case`.
3. The arms must cover every value of the scrutinee type. For `Bool`, enums,
   options (`Some(p)` and `null`), tuples and records, the checker verifies
   coverage; for `Int`, `Str`, `Decimal`, lists and other open types a final
   `_` or identifier arm is required. A guarded arm does not count toward
   coverage.
4. `case` is value branching. It does not catch failures (chapter 4).

**`for p in e { body }`** iterates a finite iterable: a list, a `Range` of
integers or dates, a `Set`, a `Map` (items are `(K, V)` tuples in key order) or a
`Relation`. The pattern is matched against each item. The loop has type `Unit`.
An unbounded `Stream` is consumed by stream operators, not `for`.

**`while c { body }`** repeats while `c` is true. Type `Unit`.

**`loop { body }`** repeats until `break`. `break v;` ends the loop with value
`v`; its type is the joined type of every `break`. A loop with no `break`
never completes normally and has the bottom type. In `for` and `while` a
`break` carries no value. `continue;` goes to the next iteration of the nearest
loop.

**`return e;`** ends the current function (or lambda) with `e`. Every `return`
and the block tail contribute to the result type.

## Expressions

### Evaluation order

1. Subexpressions evaluate left to right in source order: function and
   arguments, list elements, record fields, tuple elements, operator operands,
   string interpolations.
2. `a && b` and `a || b` evaluate `b` only when needed.
3. `a ?? b` evaluates `b` only when `a` is `null`.
4. In `if`, the condition is evaluated first, then one branch.
5. A failure in a subexpression ends evaluation of the enclosing expressions
   (chapter 4); later subexpressions do not run.

### Operators

Precedence is in chapter 1. `a < b < c` is not allowed. `a in r` tests
membership in a `Range`, `Set`, `Map` (keys) or list. `!x` needs `Bool`.
Arithmetic types are in chapter 2.

### Calls

```orna
pub fn recent(rows, within: Duration = 7.days, limit: Int = 100) =
    rows | filter(r => r.time >= now() - within) | take(limit);

recent(rows);
recent(rows, limit: 5);
recent(rows, within: 1.day, limit: 5);
```

1. A call `f(a1, ..., an)` supplies positional arguments first, then named
   arguments `label: expression`. A positional argument after a named one is an
   error.
2. Each parameter receives at most one argument. A named argument whose label
   matches no parameter is an error.
3. A parameter with a default may be omitted. Its default expression is
   evaluated at the call, after the supplied arguments, in parameter order, in
   the scope of the function's own module with the earlier parameters visible.
   An omitted parameter without a default is an error.
4. Supplied arguments evaluate in the order written, not parameter order.
5. A function has one meaning for its name; arity variants are defaults and
   type-directed variants are other names or generics. The 1.1 overloaded
   spellings remain as deprecated aliases (chapter 2).
6. Explicit type arguments are written `f<T>(args)`. Without them the type
   arguments are inferred. An argument that is a type is not allowed.
7. A function value of type `fn(A, B): R` is called positionally; labels and
   defaults belong to the declaration, not to the function type.
8. A qualified name without parentheses is the function value; with them it is
   a call. `messages.sync` is a value, `messages.sync()` a call.
9. `value.member(args)` is allowed only for a member that the receiver's type
   declares or its protocol supplies. A free function is not a method because
   its first parameter happens to match.

### Pipes

```orna
raw | decode | validate |? recover | store
```

* `v | f` is `f(v)`. `v | f(a, n: b)` is `f(v, a, n: b)`: the piped value is
  argument one.
* The stage is a postfix expression: a name, a call, a member. A lambda stage
  is parenthesised: `xs | (x => x + 1)`.
* If the left side fails, the stage is not evaluated and the failure goes on.
* `e |? h` calls `h(error)` only if `e` fails; on success it passes the value
  through. Its typing and cancellation behaviour are in chapter 4.
* `|` and `|?` are left-associative.
* Bitwise operations are ordinary functions (`std.bits.bit_or` and the like);
  `|` has no bitwise meaning.

### Fields, records, tuples, lists

* `r.name` selects a field. Every table field has a selector `Table.name`; `row.name` applies it.
* `Name { f: e, ... }` builds a nominal record or enum payload. Each field is
  checked once in written order; duplicates and unknown fields are errors;
  defaults are applied once; refinements are enforced before the value escapes.
* `{ f: e }` builds a structural record. `{}` is the empty record.
* `xs[i]` indexes a list or `Bytes`; an index out of range fails. A `Map` is read
  through a core lookup function, not `[]`.
* `(a, b)` builds a tuple and `(a,)` a one-tuple.

### Lambdas

`x => e`, `(a, b) => e`, `() => e`. Direct braced bodies follow the
classification in chapter 1. Lambda parameter types are inferred from the
context or annotated: `(x: Int) => x + 1`. A lambda is monomorphic. It captures
variables as described above.

## Patterns

| Pattern | Matches | Binds |
|---|---|---|
| `_` | anything | nothing |
| `name` | anything | `name` (an unqualified name is always a binding) |
| literal (`1`, `"a"`, `true`, `null`, dates) | an equal value | nothing |
| `Some(p)` | a non-null option whose value matches `p` | per `p` |
| `Enum.variant` | a payload-free variant | nothing |
| `Enum.variant { f, g: p }` | that variant; fields `f` and `g` | `f`, names in `p` |
| `Type { f, g: p }` | a nominal or structural record | likewise |
| `(p, q)` | a tuple of exactly that arity | per component |
| `[p, q]` | a list of exactly that length | per element |

Rules:

1. A record pattern lists the fields it uses; the shorthand `{ count }` means
   `{ count: count }`. Construction never allows the shorthand.
2. Enum variants are written qualified (`Shape.empty`). An unqualified name is a
   binding, never a variant, unless the variant was imported by name.
3. A name may be bound at most once in one pattern.
4. A literal pattern's type is the scrutinee's type; `null` needs an option.
5. A refutable pattern (anything that can fail to match) is allowed in `case`
   arms and not in `let`, parameters or lambdas.

## Changes from 1.1.0

* Closures capture variables, not values at creation time. 1.1 stated "closures
  capture values at creation time" next to reassignable `let`, which left the
  meaning of a captured, later-assigned variable undefined.
* Assigning to an outer variable from a closure is allowed. In a body started
  by `parallel`, `race`, `timeout` or `sys.start` it is a compile error and the
  body captures a snapshot.
* `x = e;` may target a variable captured from an enclosing function.
* The `|`-stage and named-argument rules are unchanged except that a type is
  passed as a type argument (`f<T>(...)`) and never as the value argument
  `as: T` (deprecated alias kept in 1.2).
* Arguments evaluate in written order; 1.1 did not state the order for named
  arguments.
* `var` stays removed; `let` is the only binding form and is reassignable.

## Decided by

D1 (typing of bodies and lambdas), D5 (calls, defaults, no overloading),
D10 (`let`, reassignment, closure slot rule, parallel snapshot rule),
D3 (`null` patterns, option joins).
