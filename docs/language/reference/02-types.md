# 2. Types

Every expression has one static type before it runs. Types are written in
annotations and inferred elsewhere. There is no dynamic `Any`.

Typing judgments are written `G |- e : T`: under the environment `G`,
expression `e` has type `T`. `G` maps names to types.

## Type forms

| Form | Meaning |
|---|---|
| `Unit`, `()` | the type with one value, `()` |
| `Bool` | `true`, `false` |
| `Int` | exact integer of unbounded size |
| `Decimal` | exact finite base-10 number |
| `Float` | IEEE-754 binary64 |
| `Str` | Unicode text (sequence of scalar values) |
| `Bytes` | immutable byte sequence |
| `Blob` | immutable binary content with media type; read as `Bytes` |
| `Uuid`, `Digest`, `Path` | identifiers and content hashes |
| `Date`, `Instant`, `LocalDateTime`, `TimeOfDay`, `ZonedDateTime`, `TimeZone` | calendar and time values |
| `Duration` | elapsed time |
| `Money<C>` | exact amount in currency `C` |
| `Error` | the value carried by a failure (chapter 4) |
| `T?` | `T` or `null` |
| `[T]` | list of `T` (also `List<T>`) |
| `Map<K, V>`, `Set<T>` | immutable persistent collections |
| `(T, U)`, `(T,)` | tuples |
| `{ a: T, b: U }` | structural record |
| `Range<T>` | ordered span of `T` |
| `fn(T, U): R` | function type |
| `Relation<T>`, `Query<T>`, `Predicate<T>`, `Stream<T>` | relational and stream types (named here, defined in their own chapters) |
| `Name`, `Name<A, B>` | nominal types, enums, refined types, table row types |

`[T]` and `List<T>` are the same type. `()` and `Unit` are the same type.

## Values are immutable

Every value of every type is immutable. A list, map, set, record or row is never
updated in place; an operation such as `push` returns a new value and leaves the
old one untouched. A variable (chapter 3) is a slot holding a value, so
`xs = xs.push(1)` replaces the slot's value. Two variables never alias mutable
state. A row read from a table is a snapshot; later table writes do not change
it.

## Numbers

### Int

`Int` is exact at any size. The literal `100000000000000000000` is an `Int`.
There are no fixed-width integer types. The engine bounds the number of digits
of any `Int` as a resource quota: a result over the quota fails with a quota
error. The quota never changes the value of a result that fits.

| Operation | Rule |
|---|---|
| `a + b`, `a - b`, `a * b` | `Int` |
| `a / b` | quotient truncated toward zero; `b == 0` fails |
| `a % b` | remainder with the sign of `a`; `b == 0` fails |
| `a ^ b` | `b >= 0` required; a negative exponent is an error |
| `<` `<=` `>` `>=` `==` `!=` | `Bool` |

`Bytes` indexing yields an `Int` in `0..255`.

### Decimal

`Decimal` is a finite base-10 value that is never routed through binary
floating point. `+`, `-`, `*` are exact. `a / b` is exact when the quotient has
a finite decimal expansion and fails otherwise. Rounded division names its scale
and rounding mode:

```orna
let third = 1.decimal.divide(3.decimal, scale: 6, rounding: half_even);
```

No ambient precision exists. Equality and order ignore trailing zeros
(`1.5 == 1.50`).

### Float

`Float` follows IEEE-754 binary64. Ordinary comparison: `NaN != NaN`, every
ordered comparison with `NaN` is false, `-0.0 == 0.0`. Sorting uses the
`totalOrder` predicate: negative NaN, `-inf`, finite negatives, `-0.0`, `+0.0`,
finite positives, `+inf`, positive NaN. Float arithmetic is never reassociated.
`Float` cannot be a key component. For a non-empty input, `min` and `max`
return NaN if any input is NaN.

### No implicit conversion

There are no implicit conversions between `Int`, `Decimal` and `Float`.

```orna
let a = 1 + 2;            // Int
let b = 1.5 + 2.25;       // Decimal
let c = 1 + 1.5;          // error: Int + Decimal
let d = Decimal.from(1) + 1.5;   // ok
let e = 1.5f + 1.5;       // error: Float + Decimal
```

Conversions are explicit calls to named core functions and are either total or
fail by name: widening `Int` to `Decimal` is total; `Decimal` to `Int` fails if
the value has a fraction; `Decimal` to `Float` and back fail if not exactly
representable unless a rounding mode is named. This chapter fixes the rule; the
spellings are declared in `core.orna`.

A numeral's type is fixed by its form (chapter 1). One literal rule is not a
conversion: a fractional literal with an expected type of `Float` is read
directly as the nearest `Float`, so `let x: Float = 0.1;` works.

### Money, Duration, Instant

`Money<C>` is exact decimal money and a library type. `C` is a nominal type that implements
`Currency` (static `code` and `minor_digits`). `12.34.GBP` is `Money<GBP>`. Adding two
different currencies, and multiplying two `Money<GBP>` values, are type errors; currency
conversion is a named, data-backed operation. A `Float` never enters money arithmetic. The
rate type `Rate<C, U>` is also a library type (chapter 5).

```orna
pub type GBP {
    impl Currency {
        static code = "GBP";
        static minor_digits = 2;
    }
}
```

`Duration` is the nominal name of the time unit family (chapter 5) and is elapsed time:
`1.day` and `7.days` are 24-hour elapsed durations, never calendar periods. An `Int` does
not flow into a `Duration` annotation; write `Duration.second(n)` or `n.second`. `Instant`
is a point on the UTC timeline.

| Operation | Type |
|---|---|
| `Instant - Instant` | `Duration` |
| `Instant + Duration`, `Instant - Duration` | `Instant` |
| `Duration + Duration`, `Duration - Duration` | `Duration` |
| `Duration * Int`, `Int * Duration` | `Duration` |
| `Money<C> + Money<C>`, `Money<C> - Money<C>` | `Money<C>` |
| `Money<C> * Decimal`, `Money<C> / Decimal` | `Money<C>` |

Calendar arithmetic uses `Period`, never `Duration`: `1.months`, `1.years` and
`Period.days(n)` (chapter 5). Calendar-day operations on an `Instant` need an explicit
`TimeZone`; a local day may have 23, 24 or 25 hours. A fixed elapsed window and a
calendar period are different operations.

## Options: `T?`

`T?` is a value of type `T` or the single absent value `null`. It is flat: there
is exactly one absent value and no nesting.

```text
(Null)  T is not an option type              =>  null : T?
(Some)  G |- e : T,  T is not an option type =>  G |- Some(e) : T?
(Lift)  G |- e : T,  T is not an option type =>  G |- e : T?
```

* `T??` is a type error. So is any instantiation that would produce it:
  calling `first<T>(xs: [T]): T?` with `T = Int?` is an error at the call.
  Where a nested absence would be needed, use a nominal type or an enum.
* Lifting is implicit: a `T` is accepted where `T?` is expected. `Some(x)` is
  the explicit spelling and produces the same value as lifting `x`.
* `null` needs an expected option type or a join with one; a bare
  `let x = null;` has an unresolved type and is an error.
* Elimination is `a ?? b` (type of `b` is the type of `a` minus the option; the
  result is `T` when `b : T`, `T?` when `b : T?`) and `case` with `Some(p)` and
  `null` patterns.
* `x == null` is `Bool` for any `x : T?`.
* No evaluation mode changes the shape of a value: a function that returns
  `Int?` returns an `Int` or `null`, never a wrapper.

```orna
fn port(raw: Str?): Int = (raw ?? "8080") | Int.parse;
```

## Collections

`List<T>`, `Map<K, V>`, `Set<T>` and `Bytes` are immutable and persistent.

* **List** `[T]` is ordered; `[1, 2, 3]` is `[Int]`. `xs[i]` indexes (an index
  out of range fails). `xs.push(x)` returns a list with `x` appended and
  `xs.concat(ys)` a list of both.
* **Map** `Map<K, V>` has unique keys. Keys use the canonical total order, so
  iteration is deterministic: ascending key order. `Float` is not a valid key.
* **Set** `Set<T>` has unique elements iterated in canonical ascending order.
* **Bytes** is a byte sequence. Reading a `Blob` yields `Bytes`.

Every type except `Float` and the types that contain one has the canonical total
order. A `Map` key or `Set` element must have it; the checker rejects any other
type there.

## Records, tuples, ranges

A **record type** `{ name: Str, age: Int }` is structural: two record types with
the same field names and types are the same type; field order is not part of the
type. A record literal always names its fields: `{ name: "Ada", age: 36 }`.

A **tuple** `(a, b)` has a fixed arity; `()` is the zero-tuple. Components
evaluate left to right and compare componentwise.

`a..b` is the half-open range `a <= x < b`; `a..=b` is closed. An endpoint may
be omitted where the context gives a meaning. Endpoints share one ordered type.
`x in range` tests membership. A range is not a uniqueness claim.

## Nominal, alias and refined types

```orna
type UserId = Uuid;                       // alias: the same type

pub type Email {                          // nominal
    value: Str,
}

type Percent = Int {                      // refined nominal
    assert >= 0;
    assert <= 100;
}
```

* `type A = B;` makes `A` another name for `B`. No new identity.
* `type A { fields }` is a new type, different from every record with the same
  fields. A field is private to the type's own `impl` blocks unless marked `pub`.
  A value can be built with `A { ... }` outside the type only when every field
  supplied is `pub`.
* `type A = B { assert ...; }` is a new type represented by `B`. Building an `A`
  evaluates every assertion in source order; a false one fails the construction.
  A refined type is distinct from its base: `Percent` is not accepted where
  `Int` is, nor the reverse.
* Equality, order, hash and display of a nominal type come from the engine's
  rules or from `impl` blocks, never from structural coincidence.
* A conversion into a type is a nested `impl From<S>`; `Target.from(source)`
  selects the one implementation whose `S` matches. Conversions are never
  applied implicitly and never chained. `From` may fail.

```orna
pub type Email {
    value: Str,
    impl From<Str> {
        fn from(raw: Str): Email {
            if raw == "" {
                fail(error(code: "email.invalid", message: "empty"));
            }
            Email { value: raw }
        }
    }
}
```

## Enums

```orna
pub enum Shape {
    circle { radius: Decimal },
    rect { w: Decimal, h: Decimal },
    empty,
}
```

An enum is closed and nominal. A payload variant is built with named fields
(`Shape.circle { radius: 2.decimal }`); a payload-free variant is `Shape.empty`.
A `case` over an enum must be exhaustive. Only payload-free enums can be key
components.

An enum may also declare `impl` blocks after its variants, as a nominal type does. A
`static` member inside one is a constant of the enum, read as `Shape.name`. Variants
must come before any `impl` block.

```orna
pub enum Rounding {
    half_up,
    half_even,

    impl Display {
        fn display(self): Str = "rounding";
    }
}
```

## Generics and protocols

```orna
pub protocol Display {
    fn display(self): Str;
}

pub fn longest<T impl Order>(xs: [T]): T? =
    xs | sort_by(x => x) | last();
```

Type parameters are declared in `<...>`; a bound is `T impl P` and several are
joined with `+`. Protocol dispatch is static. Implementations live inside the
type that owns them; two implementations of one protocol for one type that could
both apply are an error, whatever their order. There is no specialisation, no
associated type, no higher-kinded type and no run-time protocol object.

A type is passed only as a type argument, never as a value:

```orna
let m = decode<Message>(raw);       // ok
let n = decode(raw, as: Message);   // deprecated spelling
```

## Functions: no overloading

A name denotes one function. There are no overloads, for users or the platform.
A variant that differs in arity is a function with default parameters; a variant
that differs in type is a function with a different name or a generic function.

Function values have type `fn(A, B): R`. A function type is structural and
carries no names or defaults, so a call through a function value is
positional.

## Inference (D1)

Inference works inside bodies and lambdas by unification. Each unannotated
parameter, local and result gets a type variable; the rules below add equations
and the solver unifies them. A type variable still unresolved when the enclosing
function is finished is an error that names the smallest annotation that would
fix it.

```text
(Var)    x : T in G                          =>  G |- x : T
(Lit)    1 : Int   1.5 : Decimal   1.5f : Float   "s" : Str   true : Bool
(Ann)    G |- e : T,  T = declared           =>  G |- (e : T) : T
(Lam)    G, x : A |- e : R                   =>  G |- (x => e) : fn(A): R
(App)    G |- f : fn(A1..An): R,  G |- ai : Ai,  Ai = Ai'
                                              =>  G |- f(a1..an) : R
(Let)    G |- e : T                          =>  G, x : T |- (let x = e;)
(Assign) x : T in G,  G |- e : T             =>  G |- (x = e;) well-typed
(If)     G |- c : Bool,  G |- a : T,  G |- b : T   =>  G |- if c {a} else {b} : T
(Case)   every arm body : T,  patterns cover the scrutinee type
                                              =>  G |- case s {...} : T
(Pipe)   G |- v : A,  G |- f(args) : R with A as argument one
                                              =>  G |- v | f(args) : R
(Fail)   fail(e) : T for every T              (bottom type)
```

Further rules:

1. **Local `let` and lambdas are monomorphic.** A local name has one type. Only a
   declared generic function is polymorphic, and it is instantiated freshly at
   each call.
2. **Joins.** The branches of `if` and the arms of `case` must have one type
   after unification. A `T` and a `T?` join to `T?`. A branch that always
   fails, returns, breaks or continues has the bottom type and joins with
   anything.
3. **`pub` functions need full annotations**: every parameter type and the result
   type. A private function may omit parameter types; the types are solved from
   the body and from default values, not from call sites. An unresolved variable
   is an error ("annotate parameter `x`"). Result types are always inferred
   from the body when omitted.
4. **Operators** are checked after their operands are solved. An arithmetic
   operator needs both operands of one numeric type (or the pair of types in
   the Money, Duration and Instant tables above). A variable never resolved to
   a type that supports the operator is an error, so `fn double(x) = x + x;` is
   rejected and `fn inc(x) = x + 1;` gives `fn(Int): Int`.
5. **Expected types flow inward** only for literals and `null` and to resolve
   record and constructor targets; they never insert a conversion.
6. **Annotations are redundant when they match.** Adding an annotation equal to
   the inferred type never changes behaviour.
7. **Inference is value-independent.** It reads the resolved source and the
   dependency snapshot and nothing else, so it yields the same types on every
   machine. Errors list the conflicting constraints with their spans.
8. **Published signatures.** The inferred signature of a function is recorded in
   metadata exactly as if written out. Historical execution uses the signatures
   of the historical snapshot.

```orna
fn clamp(x, lo: Int, hi: Int) =
    if x < lo { lo } else if x > hi { hi } else { x };
// x : Int, result : Int, solved from comparisons with lo and hi

pub fn total(items: [Int]): Int = items | sum();     // full annotations required
```

## Changes from 1.1.0

* `Int` stays unbounded; the digit cap is a quota error, not a language rule. No
  fixed-width types.
* The numeric tower has no implicit conversions; conversions are named and
  total or failing. 1.1 stated no general rule.
* `T?` is one flat representation with `null` as its only absent value. `T??` is
  an error. 1.1 wrote the type as `Option<T>` and did not state that options do
  not nest.
* Added `Map<K, V>`, `Set<T>`, `Bytes`; `List` gains `push` and `concat`.
* Unit families (chapter 5) replace the 1.1 user `dim` and `unit` declarations and the
  checker's dimensional algebra. `Quantity<N, U>` is a type over a unit key.
  `Money<C>`, `Duration`, `Instant` and `Decimal` stay with their literal postfixes.
* `Money<GBP> / kWh` has no kernel type product. It is the library `Rate<GBP, kWh>`
  (chapter 5); the kernel has no currency axis.
* Inference is unification-based. `pub` functions require full annotations and
  private parameters left unresolved are errors; 1.1 made every annotation
  optional. Private
  parameters are solved from the body, not from call sites.
* No overloading. Type-directed variants are distinct functions or generics; a
  type is a type argument (`decode<T>(raw)`), not a value argument (`as: T`,
  deprecated alias kept in 1.2).
* `Result`, `Ok`, `Err` remain non-core, and `std.result` is deleted.

## Decided by

D1 (inference rules), D3 (flat options), D5 (no overloading, type arguments),
D6 (`Map`, `Set`, `Bytes`), D7 (`Int` quota, no implicit numeric conversion),
D8 (units as enum families, amended 2026-11-10; `Duration`, `Instant`, `Decimal`, `Money<C>` stay), D4 (`std.result`
deleted).
