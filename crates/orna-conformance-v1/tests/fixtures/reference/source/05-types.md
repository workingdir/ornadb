# 5. Types, numbers and nominal values {#types}

## Core type forms

The following forms belong to the intrinsic type environment. Context and presentation interfaces are defined in their owning chapters; this list is not permission to introduce unversioned intrinsic types.

```text
Unit
Bool
Int
Float
Decimal
Str
Blob
Path
Digest
Uuid
Date
Instant
LocalDateTime
TimeOfDay
Duration
TimeZone
ZonedDateTime
Error
Range<T>
List<T>       written [T]
Option<T>     written T?
Relation<T>
Query<T>
Predicate<T>
Stream<T>
Money<C>
Quantity<N, U> or surface forms such as Float<kWh>
record types
function types
nominal enum types
nominal user types
refined nominal types
nominal table row types
stored table-reference types
protocol-constrained generic types
```

**ORNA-TYPE-001** `T?` MUST mean `Option<T>` and MUST NOT mean an unchecked nullable value.

**ORNA-TYPE-002** A failable expression has a successful value type and a separate abrupt failure channel carrying an `Error`; failure is not represented by `Result<T, E>` in source.

**ORNA-TYPE-003** Automatic failure propagation MUST NOT silently discard a stream item. Stream retry, preservation, skip and dead-letter behavior remains explicit through the stream/checkpoint model.

**ORNA-TYPE-004** Implementations MUST NOT use the same type name for raw bytes and a byte-count quantity.

**ORNA-TYPE-005** User-defined closed variants use nominal `enum` declarations. Orna does not use the expression-pipeline token `|` as a type-union or variant-list operator.

**ORNA-TYPE-006** `Result`, `Ok`, and `Err` are not reserved core type constructors or variants in 1.0.

**ORNA-TYPE-007** A user-defined `type` declaration is either a transparent alias, a nominal type, or a refined nominal type as specified in [aliases, nominal types and refinements](#expressions).

**ORNA-TYPE-008** The absence of a source annotation does not make a value dynamically typed. A conforming processor MUST infer a static type or issue a type-inference diagnostic.

## Value and binding model

Orna values are immutable. A local name introduced by `let` is a function-local slot whose binding may be reassigned by an assignment statement; reassignment replaces the entire immutable value and does not expose shared mutable object identity.

**ORNA-VALUE-001** Primitive, list, record, enum, option, quantity, money, nominal and row values MUST have value semantics.

**ORNA-VALUE-002** A row returned from a query is an immutable snapshot containing its table identity, primary key, snapshot context and logical fields. A later table update MUST NOT mutate an already-held row value.

**ORNA-VALUE-003** Closures capture values at creation time. Table handles retain their database/snapshot context unless the program explicitly selects another context.

**ORNA-VALUE-004** Cyclic ordinary list/record/nominal values are not constructible in version 1.0. Persistent graph cycles may exist through table references.

**ORNA-VALUE-005** Memory management, pointer identity and finalization timing MUST NOT be observable language semantics.

**ORNA-VALUE-006** Reassigning a `let` binding MUST NOT mutate any value previously captured, returned, stored or observed through another binding.

**ORNA-VALUE-007** `var` is not a 1.0 binding declaration. A processor that recognizes the prohibited declaration context MUST emit `ORNA091-E-VAR` and propose `let`.

## Numeric types and exact decimals

**ORNA-NUM-001** `Int` MUST represent exact integers.

**ORNA-NUM-002** `Decimal` MUST represent finite exact base-10 values with implementation resource limits but without binary-float conversion.

**ORNA-NUM-003** `Float` MUST use IEEE-754 binary64 semantics and MUST NOT be used as the canonical representation of money.

**ORNA-NUM-004** Decimal addition, subtraction and multiplication MUST be exact unless an implementation resource limit is exceeded, in which case evaluation fails with a typed `Error`.

**ORNA-NUM-005** Decimal arithmetic MUST remain exact where the mathematical result has a finite decimal representation. Division whose result is not a finite decimal MUST fail unless an explicitly selected rounding/precision operation is used.

```orna
1.decimal.divide(
    3.decimal,
    scale: 6,
    rounding: half_even,
)
```

No process-global or ambient decimal precision changes the meaning of source text.

## Float equality, sorting and aggregation

**ORNA-FLOAT-001** Ordinary Float equality and ordered comparison follow IEEE-754 binary64 semantics: `NaN != NaN`, every ordered comparison involving `NaN` is false, and `-0.0 == 0.0`.

**ORNA-FLOAT-002** Sorting Float values MUST use the IEEE 754 `totalOrder` predicate for binary64 values. In ascending order, negative NaNs precede negative infinity, finite negative values and `-0.0`; `-0.0` precedes `+0.0`; positive finite values and positive infinity follow; and positive NaNs come last. Signaling/quiet NaNs and distinct NaN payloads are ordered exactly as required by `totalOrder`.

**ORNA-FLOAT-003** `Float` MUST NOT be accepted as a primary-key component or default hash-key type.

**ORNA-FLOAT-004** Float arithmetic MUST NOT be silently reassociated or evaluated with unspecified fast-math transformations.

**ORNA-FLOAT-005** A Float aggregate MUST process rows in the relation's observable order. Empty `sum` returns additive zero; empty `min`, `max` and `mean` return `null`.

**ORNA-FLOAT-006** Float sorting order and ordinary equality are distinct relations. A sort MUST distinguish `-0.0` from `+0.0` and MAY distinguish NaN bit patterns even though ordinary equality treats the two zeros as equal and every NaN as unequal to every value, including itself.

**ORNA-FLOAT-007** For a non-empty Float input, `min` and `max` return the canonical NaN if any input is NaN. Otherwise they use numeric order, with `min` choosing `-0.0` when either zero sign is present and `max` choosing `+0.0`. Libraries MAY provide explicitly named finite-only or NaN-ignoring alternatives.

### Algorithm FLOAT-TOTAL-1

For a binary64 value, interpret its exact 64 bits as an unsigned integer `bits` and compute an unsigned sortable key:

```text
if the sign bit is 1: key = bitwise_not(bits)
otherwise:             key = bits xor 0x8000000000000000
```

Compare keys as unsigned 64-bit integers. This is the normative bit-level implementation of the ordering required by `ORNA-FLOAT-002`; it orders every finite value, infinity, signed zero, signaling NaN, quiet NaN and NaN payload deterministically. The analogous binary32 algorithm uses the mask `0x80000000`. Implementations MAY use another algorithm only when every value pair produces the same order.

## Blob and information quantity

`Blob` is raw binary content. Storage size is a numeric quantity with an information unit.

```text
payload: Blob
used: Float<GiB>
```

## Time model

`Instant` is an absolute point on the UTC timeline. `LocalDateTime` is civil time without a zone. `ZonedDateTime` is a local date-time plus an IANA time zone and resolved offset. `Duration` is elapsed time.

**ORNA-TIME-001** Telemetry timestamps SHOULD use `Instant`.

**ORNA-TIME-002** Calendar-day operations over `Instant` values MUST require or derive an explicit time zone.

Invalid:

```orna
readings | bucket_by(1.day)
```

when `readings.time` is an `Instant` and no zone can be derived.

Valid:

```orna
readings | bucket_by(1.day, zone: Europe.London)
```

**ORNA-TIME-003** Calendar-day bucketing MUST respect daylight-saving transitions; a local day MAY contain 23, 24 or 25 hours.

**ORNA-TIME-004** Fixed elapsed-time windows and calendar periods MUST be distinct operations even if their textual durations appear similar.

## Ranges

`Range<T>` is an ordinary ordered span. It does not imply non-overlap or temporal uniqueness merely because it is stored in a table.

```text
1..5
2026-01-01..2027-01-01
start..
..end
1..=5
```

**ORNA-RANGE-001** `a..b` constructs a half-open range containing values `x` for which `a <= x && x < b`.

**ORNA-RANGE-002** `a..=b` constructs an inclusive range containing values `x` for which `a <= x && x <= b`.

**ORNA-RANGE-003** Either endpoint MAY be omitted where the surrounding operation supplies a type and meaningful unbounded side.

**ORNA-RANGE-004** Range endpoints MUST have one compatible ordered type. Empty ranges are valid ordinary values.

**ORNA-RANGE-005** Ranges have a total order only when `T` has a total order: lower endpoint (unbounded first), then upper endpoint (unbounded last), then exclusivity before inclusivity at equal endpoints. This ordering is for sorting/serialization and does not imply overlap uniqueness.

**ORNA-RANGE-006** `value in range` tests membership. Range iteration/slicing behavior is supplied by the relevant iterable/index operation rather than by special grammar.

**ORNA-RANGE-007** Version 1.0 does not redefine primary-key equality as range overlap. Temporal exclusion/non-overlap constraints are represented by explicit table or cross-table assertions, not by making overlap masquerade as equality.

## Dimensions and units

```orna
pub dim Length;
pub dim Mass;
pub dim Time;
pub dim Temperature;
pub dim Speed = Length / Time;
pub dim Energy = Mass * Length^2 / Time^2;
pub dim Power = Energy / Time;

pub unit m : Length base;
pub unit kg : Mass base;
pub unit s : Time base;
pub unit K : Temperature base;
pub unit km : Length = 1000.m;
pub unit mile : Length = 1609.344.m;
pub unit min : Time = 60.s;
pub unit hour : Time = 60.min;
pub unit J : Energy = kg * m^2 / s^2;
pub unit kWh : Energy = 3.6e6.J;
pub unit mph : Speed = mile / hour;
pub unit C : Temperature = K offset 273.15 affine;
pub unit deltaC : Temperature = K;
```

**ORNA-UNIT-001** Unit compatibility is determined from reduced dimension exponents, rational scale and affine behaviour, not from unit name alone.

**ORNA-UNIT-002** Compatible units from attached databases interoperate when their structural definitions are equivalent.

**ORNA-UNIT-003** Aggregation preserves dimensions according to the aggregate's algebra.

**ORNA-UNIT-004** Adding quantities of incompatible dimensions is a compile-time error where types are known.

**ORNA-UNIT-005** For an affine unit `U`, an absolute value and its corresponding linear delta quantity obey point/vector algebra:

```text
Absolute<U> - Absolute<U> -> Delta<U>
Absolute<U> + Delta<U>    -> Absolute<U>
Delta<U> + Absolute<U>    -> Absolute<U>
Absolute<U> + Absolute<U> -> error
scalar * Absolute<U>      -> error
scalar * Delta<U>         -> Delta<U>
```

```text
30.C - 20.C        // 10.deltaC
20.C + 5.deltaC    // 25.C
20.C + 5.C         // invalid
```

**ORNA-UNIT-006** Integration reduces dimensions algebraically; integrating speed over time yields length.

**ORNA-UNIT-007** Aggregation over affine absolute quantities follows this matrix:

- selection/order operations (`first`, `last`, `min`, `max`, `median`, percentile and mode) are permitted and return an absolute value;
- `sum` is invalid for absolute affine values and valid for delta values;
- non-empty `mean` is `base + mean(value - base)` for any selected base value in the input and returns an absolute value;
- `range` and standard deviation return the corresponding delta quantity;
- variance returns the square of the corresponding delta quantity;
- differences and derivatives operate on delta quantities;
- integration of an affine absolute quantity is invalid unless the caller first selects a meaningful linear reference.

**ORNA-UNIT-008** Unit suffix resolution MUST be static and unambiguous in the current import context. A unit suffix does not perform an arbitrary `From` conversion.

## Money and currencies

`Money<C>` is an exact decimal monetary amount whose currency is a nominal type implementing `Currency`.

```text
pub protocol Currency {
    static code: Str;
    static minor_digits: Int;
}

pub type GBP {
    impl Currency {
        static code = "GBP";
        static minor_digits = 2;
    }
}

amount: Money<GBP>
rate: Money<GBP> / kWh
let price = 12.34.GBP;
```

**ORNA-MONEY-001** Money MUST use exact decimal semantics.

**ORNA-MONEY-002** Two amounts with different currency parameters MUST NOT be added without an explicit data-backed conversion.

**ORNA-MONEY-003** Dimensional algebra MUST reduce an exact energy quantity such as `Decimal<kWh> * (Money<GBP> / kWh)` to `Money<GBP>`. A binary `Float` quantity MUST NOT enter an exact Money calculation without an explicit decimal conversion, scale and rounding rule.

**ORNA-MONEY-004** A currency nominal type MUST implement `Currency` with stable static properties `code: Str` and `minor_digits: Int`. A currency symbol is deliberately not part of the protocol.

**ORNA-MONEY-005** Currency conversion whose rate changes over time MUST require an effective instant or date and MUST be an explicit data-backed operation.

```orna
rates.convert(invoice.total, GBP, on: invoice.date)
```

**ORNA-MONEY-006** Minor-unit precision controls conventional display and settlement quantization, not the internal precision of intermediate calculations.

**ORNA-MONEY-007** Canonical JSON encoding of money SHOULD use a decimal string and currency code, for example `{ "amount": "12.34", "currency": "GBP" }`.

**ORNA-MONEY-008** The grammar MUST NOT provide a special `currency` declaration. That invalid declaration receives `ORNA091-E-CURRENCY` with a rewrite to a nominal type and nested `Currency` implementation.

**ORNA-MONEY-009** Formatting chooses symbols, placement, spacing, grouping and decimal punctuation from an explicit or activation-derived locale/format context. Presentation MUST NOT change money equality, storage, hashing or canonical serialization.

**ORNA-MONEY-010** `12.34.GBP` constructs `Money<GBP>` exactly and MUST NOT parse through binary `Float`.

## Inference-first static typing

The compiler infers omitted type annotations from literals, operators, calls, fields, control-flow joins, assignments, protocol selection, return paths and contextual expectations.

```orna
fn square(x) = x * x;

pub fn total(items: [LineItem]): Money<GBP> =
    items | map(item => item.price * item.quantity) | sum();
```

**ORNA-INFER-001** Omitted annotations MUST be inferred to one static type before evaluation.

**ORNA-INFER-002** Type inference MUST NOT silently fall back to a dynamic `Any` type.

**ORNA-INFER-003** Function parameter and return annotations are optional when the signature is unambiguously inferable.

**ORNA-INFER-004** Public functions SHOULD annotate externally meaningful parameter and return types, but absence of those annotations is not a syntax error when inference succeeds.

**ORNA-INFER-005** A compiler MUST expose the fully inferred exported signature through metadata, diagnostics and `sys.Function` exactly as if it had been written explicitly.

**ORNA-INFER-006** Inference MUST be independent of runtime data values and MUST produce the same signature for the same resolved source and dependency snapshot.

**ORNA-INFER-007** When inference is underconstrained or recursive constraints do not converge, the diagnostic MUST identify the smallest useful annotation site rather than demanding annotations everywhere.

**ORNA-INFER-008** Contextual numeric and collection inference MUST preserve the exactness rules of [numeric exactness](#types) and [literal forms](#lexical).

**ORNA-INFER-009** Protocol selection and nested implementations participate in inference, but overlapping implementations remain invalid.

**ORNA-INFER-010** Adding a redundant annotation MUST NOT change successful program behavior.

## Transparent aliases and nominal types

The `type` declaration defines aliases, nominal types and refined types.

Transparent alias:

```orna
type UserId = Uuid;
```

Nominal type:

```orna
pub type EmailAddress {
    value: Str,

    impl From<Str> {
        fn from(value) {
            if !valid_email(value) {
                fail(error(
                    code: "email.invalid",
                    message: "invalid email address",
                ));
            }

            EmailAddress { value: value }
        }
    }

    impl Display {
        fn display(self, context) = self.value;
    }
}
```

**ORNA-NOMINAL-001** `type Name = Existing;` declares a transparent alias with no new runtime or equality identity.

**ORNA-NOMINAL-002** `type Name { ... }` declares a nominal type distinct from every structural record with the same fields.

**ORNA-NOMINAL-003** `type Name = Base { ... }` declares a refined nominal type represented by `Base` and distinct from `Base`.

**ORNA-NOMINAL-004** Orna 1.0 has no separate `opaque` type declaration.

**ORNA-NOMINAL-005** A nominal field is private by default. Prefixing the field with `pub` exposes that field through the type's public representation.

**ORNA-NOMINAL-006** Private fields are accessible to the owning type's nested implementations and inaccessible to unrelated modules.

**ORNA-NOMINAL-007** Direct construction outside the owning type is permitted only when every supplied field is publicly constructible; otherwise callers use an exposed conversion or method.

**ORNA-NOMINAL-008** Field visibility controls representation access, not value mutability. Nominal values remain immutable.

**ORNA-NOMINAL-009** Nominal equality, order, hashing, display, presentation and codec behavior are supplied by explicit core rules or protocol implementations; structural coincidence alone does not grant them.

**ORNA-NOMINAL-010** A formatter SHOULD keep nested implementations adjacent to the type whose representation they can access.

## Refined types

A refined type attaches always-enforced assertions to a base type:

```orna
type Meter = Int {
    assert >= 0;
    assert <= 100;
}
```

The declaration is read as “a `Meter` is an `Int` for which each proposition holds”. The candidate base value is the assertion owner subject; it is not exposed as a general source variable.

**ORNA-REFINE-001** Constructing or converting into a refined type MUST evaluate every declaration assertion in source order.

**ORNA-REFINE-002** A false refined-type assertion prevents construction and fails with a safe, source-linked assertion diagnostic.

**ORNA-REFINE-003** A refined-type assertion MAY use a subjectless comparison such as `>= 0`, which elaborates to a predicate over the candidate base value.

**ORNA-REFINE-004** The removed `where self` spelling is invalid and receives `ORNA-A091-001`.

**ORNA-REFINE-005** Refined-type assertions are enforced in all build and runtime modes; they are not debug-only.

## Explicit conversions

The target type owns its conversions through nested `From<Source>` implementations:

```orna
pub type EmailAddress {
    value: Str,

    impl From<Str> {
        fn from(value) {
            if !valid_email(value) {
                fail(error(code: "email.invalid", message: "invalid email"));
            }
            EmailAddress { value: value }
        }
    }

    impl From<EmailHeader> {
        fn from(header) = EmailAddress.from(header.address);
    }
}
```

**ORNA-CONVERT-001** `Target.from(source)` selects a `From<Source>` implementation nested in `Target`.

**ORNA-CONVERT-002** A nominal type MAY implement `From` for several distinct source types.

**ORNA-CONVERT-003** Two implementations whose source types overlap for the same target are invalid.

**ORNA-CONVERT-004** `From` is allowed to fail through the ordinary failure channel. Orna does not split the surface into `From` and `TryFrom`.

**ORNA-CONVERT-005** `TryFrom` is not a core 1.0 protocol and tooling SHOULD propose `From` when it recognizes the legacy design spelling.

**ORNA-CONVERT-006** Arbitrary `From` implementations are not applied implicitly. Source writes the conversion or uses a syntax whose semantics explicitly names the target, such as `12.34.GBP`.

**ORNA-CONVERT-007** A compiler MUST NOT discover or execute an implicit multi-step conversion chain.

**ORNA-CONVERT-008** Lossy, policy-dependent, locale-dependent or data-backed transformations MUST use a specifically named operation rather than a generic `From` implementation.

**ORNA-CONVERT-009** A conversion implementation may access private fields of its target because it is lexically owned by that target.

**ORNA-CONVERT-010** Conversion failure preserves the original error cause and aborts the activation unless recovered by `|?`.



## Inference boundary algorithm

### Algorithm INFER-1

1. Parse declarations without inventing dynamic types for omitted annotations.
2. Create type variables for omitted parameter, local and return types.
3. Add constraints from literals, patterns, calls, operators, assignments, fields, case arms, returns, table schemas, protocol requirements and contextual types.
4. Resolve imported/exported signatures and nested implementation candidates from the captured dependency snapshot.
5. Solve constraints to a principal type where one exists.
6. Reject overlap, unsatisfied protocol bounds, incompatible assignments and ambiguous unresolved variables.
7. For recursion, iterate strongly connected declaration components; request a targeted annotation only when no stable principal solution exists.
8. Persist the inferred public signature and failure metadata in semantic catalogue output.

**ORNA-INFER-011** Inference errors MUST show the conflicting constraints and their source spans.

**ORNA-INFER-012** Refactoring a private helper from an explicit type to an equivalent inferred type MUST NOT change exported semantic identity by itself.

**ORNA-INFER-013** Semantic diffs SHOULD distinguish an annotation-only edit from an inferred signature change.

**ORNA-INFER-014** Historical execution uses the inferred signatures captured by the historical code snapshot, not signatures re-inferred from current source.

## Nested implementation ownership

A nested implementation's target is the immediately enclosing nominal type. There is no need to repeat it:

```orna
pub type Username {
    value: Str,

    impl Display {
        fn display(self, context) = self.value;
    }
}
```

**ORNA-IMPL-002** A nested `impl P` semantically means implementation of protocol `P` for the immediately enclosing nominal type.

**ORNA-IMPL-003** Nested implementations are not allowed inside a transparent alias because the alias has no separate nominal ownership.

**ORNA-IMPL-004** A refined nominal type MAY contain nested implementations in the same body as its assertions.

**ORNA-IMPL-005** Protocol implementation lookup is based on stable semantic identities, not merely source names.

**ORNA-IMPL-006** Renaming a type through semantic rename preserves its nested implementation ownership.

## Conversion selection algorithm

### Algorithm CONVERT-1

For explicit `Target.from(source)`:

1. Resolve `Target` to one nominal target type.
2. Infer/check the source's static type `S`.
3. Find nested implementations of `From<S'>` in `Target` for which `S` equals or validly instantiates `S'`.
4. Require exactly one most-specific non-overlapping implementation; specialization itself is not supported.
5. Invoke its `from` function.
6. If invocation fails, propagate normally. If it succeeds, require the target value type.
7. Do not search for an intermediate `U` and do not chain `Target.from(U.from(source))` implicitly.

