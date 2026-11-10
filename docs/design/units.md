# Units, dimensions and dimensional algebra

Status: decided. AMENDED by owner direction 2026-11-10 (`docs/language/DECISIONS.md`,
D8 amendment; D15). The surface is enum-shaped unit families (D15, section 2). The same
owner direction decides four sub-questions: `day` and `days` are the elapsed 24-hour
`Duration` and calendar arithmetic uses `months`, `years` and `Period.days(n)` (Q1);
`Duration` is the nominal name of the time family (Q5); an `Int` does not flow into a
`Duration` annotation (Q6); `Money<C>` is a library type with a library `Rate<C, U>`, and
the kernel has no currency axis (O4). Sub-questions Q2, Q3, Q4 and Q7 and kernel-review
changes C4 to C7 and C11 are settled by the default rule of section 12. The owner may
reverse any default. The document is a clean-sheet specification. It does not port the
1.1.0 implementation in `orna-check/src/units.rs`. It is the input for HIR stages
H2c-1 and H2c-2 (section 11.2), implemented in `orna-hir`.

Clause numbers: `ORNA-UNIT-001` to `008` are the 1.1.0 clauses, carried forward with
their 1.1.0 meaning unless marked withdrawn. Section 6 says how each is realised.
`ORNA-UNIT-009` onward are new in 1.x.

Dogfooding principle. Built-in unit families and the money types are ordinary Orna
declarations, written in the same form a user writes. A reader who opens the source of
`Duration` or of a unit family reads plain Orna. The checker knows natively only the
minimum listed in section 5. Everything else is library Orna. Section 8 writes the
embedded `core` declarations out in full.

## 1. Summary

* A **unit family** is an enum. A **unit** is a payload-free variant of a family.
* A **base family** has exactly one bare variant, the base unit. Its dimension is the
  axis of that unit. Other variants give scales: `kilometre = 1000 metre`.
* A **derived family** has no bare variant. Its variants are unit expressions, and its
  dimension is computed from them: `kmh = Length.kilometre / Duration.hour`.
* A **dimension** is a name for an exponent vector over **base units**.
* A **unit key** is an exponent vector and an exact positive rational scale. Two units
  with equal keys are the same unit for typing.
* A **quantity type** is `Quantity<N, U>`: a numeric type `N` in `{Int, Decimal, Float}`
  and a unit key `U`. The surface spellings `Decimal<U>` and `Float<U>` are sugar for
  `Quantity<Decimal, U>` and `Quantity<Float, U>`.
* Addition, subtraction and comparison need equal exponent vectors. Mixed units of one
  dimension meet at the finer unit (ORNA-UNIT-011).
* Multiplication, division and integer powers combine keys and never touch stored values.
* A dimensionless result is a plain numeric type. Its scale is folded into the value
  exactly, or it is a compile error (ORNA-UNIT-014).
* A family name in a type position is a membership type: `Duration` accepts the
  quantities of its units and no bare number (ORNA-UNIT-020).
* Money, rates and affine types are library types (sections 8.3 and 8.5). Conversion
  `to` is a library function (ORNA-UNIT-015).

## 2. Declarations (surface)

The surface is the enum form, decided by owner direction (D15). The keyword forms `dim`
and `unit` of the first draft of this document are withdrawn. Built-in and user families
take the same form.

```orna
pub enum Length {
    metre,                               // the base unit: the one bare variant
    kilometre = 1000 metre,
    mile = 1609.344 metre,
}

pub enum Duration {                      // the time family; the nominal name of its quantities
    second,
    s = second,
    seconds = second,
    min = 60 second,
    minute = 60 second,
    minutes = 60 second,
    hour = 3600 second,
    hours = 3600 second,
    day = 86400 second,                  // elapsed 24 hours, not a calendar day (Q1)
    days = 86400 second,
}

pub enum Speed {                         // derived: no bare variant
    kmh = Length.kilometre / Duration.hour,
    mph = Length.mile / Duration.hour,
}
```

Calls and literals, same for built-in and user families:

```orna
Length.kilometre(5)      // a Quantity<Int, Length.kilometre>, the same as 5.kilometre
5.kilometre              // the suffix form (section 6, ORNA-UNIT-008)
```

Grammar. The productions replace the enum body of the 1.2 grammar for unit families; the
payload form is unchanged. Unchanged productions of the 1.2 grammar are not repeated.

```text
enum_declaration := visibility 'enum' NAME generic_params? '{' enum_body? '}'
enum_body        := enum_variant (',' enum_variant)* ','?
enum_variant     := contextual_name ( '{' payload_fields? '}' )?      (1.2 payload form, unchanged)
                  | contextual_name ( '=' unit_expr )?                (unit variant; no '=' is the base unit)
unit_expr        := unit_term ( ('*' | '/') unit_term )*              (left to right; '/' binds one term)
unit_term        := RATIONAL? unit_name ( '^' INT )?
unit_name        := NAME | NAME '.' NAME                              (family-qualified variant)
```

Rules on the surface:

* A variant is either payload (1.2 enum) or unit (unit family). A family may not mix
  payload variants with unit variants.
* `RATIONAL` is an exact decimal literal such as `0.125`. Fractions are not written.
  A non-terminating ratio is not a valid scale.
* `unit_name` in a unit expression resolves by the static unit lookup of ORNA-UNIT-008.
  A bare name needs the variant in scope; `Family.variant` always resolves.
* There is no `canonical`, `repr`, `offset`, `affine`, `dim`, `unit`, `base` or `alias`
  clause or declaration. A transparent alias is `type Name<..> = ..;` (U03). Wire forms
  are bound by the engine registry (section 8.4, P9); affine types are library types
  (section 8.5). Section 11.2 says when the five words stop being reserved.
* Rejected: a base family with zero or two bare variants (ORNA-UNIT-009); a derived
  variant with no `=`; a unit family whose variants do not share a dimension
  (ORNA-UNIT-010).

Unit variants are singleton types (P3e). `Length.kilometre` is a value of the enum type
`Length`, and `Quantity<Decimal, Length.kilometre>` names a unit. Values of a unit family
carry no run-time data: the key is static (ORNA-UNIT-019).

## 3. Core model

### 3.1 Axes and exponent vectors

```text
Axis      := the canonical DefId of a base unit
Exponents := finite map Axis -> Int, zero entries removed, each Int in [-32, 32]
```

Addition is pointwise; negation and integer scalar multiplication are pointwise. The zero
map is dimensionless. A currency is not an axis (O4).

### 3.2 Rational scale

Scales are exact rationals `p/q` over big integers, with `q > 0` and `gcd(p, q) = 1`. A
scale is positive.

### 3.3 Unit key

```text
UnitKey := (V: Exponents, s: Rational > 0)
```

* A base unit `metre` of axis `metre` has key `({metre: 1}, 1)`.
* `kilometre = 1000 metre` has key `({metre: 1}, 1000)`.
* `Duration.hour` has key `({second: 1}, 3600)`.
* A unit expression `u1 * u2^n / u3` has exponents that add with `n`; scales multiply
  with `s_i ^ n`; a literal factor multiplies the scale.
* A unit key is a function of its expression only. Names are not in the key.

### 3.4 Quantity type

```text
Quantity<N, U>   N in {Int, Decimal, Float}, U a UnitKey interned per program
```

There are no affine keys and no point or delta types in the kernel (ORNA-UNIT-005 is
withdrawn; section 8.5 gives the library form).

### 3.5 Dimension of a key

`dim(U) = U.V`. The scale is ignored. Two keys are of the same dimension when their
exponent maps are equal. The dimension of a family is the dimension of its variants
(ORNA-UNIT-010).

## 4. Canonical form, printing, bytes

The canonical form of a quantity type is `(N, U)` with `U` as in 3.3. The type carries the
unit key, not a family name, so `Speed.kmh` and `Length.kilometre / Duration.hour` give
one type.

Printing is total and deterministic:

1. If a declared unit has key `U`, print the name of the smallest canonical DefId among
   the candidates (the family-qualified variant name).
2. Otherwise, if `U.s = 1`, print the axes in ascending canonical DefId order, positive
   exponents as numerator and negative as denominator, for example `metre/second` or
   `metre^2`.
3. Otherwise print `(S*` followed by (2) and `)`, where `S` is the scale as `p` or `p/q`.

A family name prints by the same rule. A base family prints its base unit.

Canonical bytes of a unit key, for attached-database equivalence (ORNA-UNIT-002):

```text
tag 0x55 | count:u32 | for each axis in ascending canonical name:
  name-len:u32 name-utf8 exponent:i32 | s.num s.den (signed decimal strings,
  big-endian length-prefixed)
```

Stored values hold no unit bytes. A column's type carries the key.

## 5. Native minimum versus library Orna

The checker knows natively:

1. Exponent-map arithmetic and rational arithmetic (section 3).
2. The `Quantity<N, U>` type constructor, unit-key reduction of unit expressions, and
   dimension equality.
3. The interface, kept under every option:
   a. `Quantity<N, U>` over `N` in `{Int, Decimal, Float}` and a key `U`;
   b. keys: an exponent map over base units and an exact rational scale; key arithmetic
      for `*`, `/` and `^n` with an integer literal exponent;
   c. exactness and ratio queries: for two keys of one dimension, the finer key, and the
      exact ratio or "inexact". Used by mixed-unit `+`, `-` and comparison, and by the
      library `to`;
   d. two casts: `erase`, from `Quantity<N, U>` to its number `N`, and `unerase<U>`, from
      `N` to `Quantity<N, U>`. Library code reads and builds a quantity only through them;
   e. unit-name resolution: the unit-name step of ORNA-UNIT-008 yields a key for a unit
      name in scope.
4. The declaration pass for unit families: the variant scales and the dimension of each
   family (base or computed). No other header clause exists.
5. Family membership for type annotations (ORNA-UNIT-020).
6. The typing rules ORNA-UNIT-001 to ORNA-UNIT-020, those not withdrawn, in one module.

Everything else is library Orna: the time units and the calendar `Period`, the size units,
the currency protocol, `Money<C>`, `Rate<C, U>`, affine point and delta types, `to` with
its rounding, aggregates, `Display`, and wire forms through the P9 registry. Section 8
lists the `core` declarations. A user family takes exactly the same form.

The checker does not know the names `hour`, `min`, `s`, `day`, `days`, `Duration`,
`Period`, `months`, `years`, `Money`, `Rate`, `GBP` or `byte`. They are found by lookup
like any other name.

## 6. Typing rules

Notation: `Q_i = Quantity<N_i, U_i>`, `V_i = dim(U_i)`, and `r = s_1 / s_2` is the scale
ratio of two keys. An exact ratio is a rational whose decimal expansion terminates.

**ORNA-UNIT-001 (compatibility, carried forward).** Two unit keys are compatible when
their reduced exponents and rational scale agree. Names do not take part. Realised by key
equality (3.3).

**ORNA-UNIT-002 (structural equivalence, carried forward).** Compatible units from
attached databases interoperate when their structural definitions are equivalent.
Realised by the canonical bytes of section 4.

**ORNA-UNIT-003 (aggregation preserves dimension, carried forward).** An aggregate has the
dimension its algebra gives. Order statistics and sums keep the operand key; products and
quotients follow ORNA-UNIT-012.

**ORNA-UNIT-004 (incompatible addition, carried forward).** Adding, subtracting or
comparing quantities of different dimensions is a compile-time error where types are
known: `ORNA-UNIT-DIM`, naming both printed dimensions. Realised by ORNA-UNIT-011.

**ORNA-UNIT-005 (affine point and vector): WITHDRAWN from the kernel (C6).** The kernel has
no offset and no affine flag. Affine point and delta types are library types (section
8.5). The clause number is kept so that 1.1.0 references stay resolvable.

**ORNA-UNIT-006 (integration, carried forward).** Integration reduces dimensions
algebraically: the key of the integrand times the key of the variable.

**ORNA-UNIT-007 (aggregation, carried forward, non-affine part).** Selection and order
operations (`first`, `last`, `min`, `max`, `median`, percentile and mode) return the
operand key. `sum`, `mean`, `range` and standard deviation keep the operand key; variance
returns the square of the key. Differences and derivatives operate on the key of their
operands. Integration follows ORNA-UNIT-006. The affine rows of the 1.1.0 matrix are
library behaviour (section 8.5).

**ORNA-UNIT-008 (static suffix resolution, carried forward with the P2 lookup).** A
numeric literal `n.name` is resolved statically and without ambiguity, in this order:

1. A variant of a unit family in scope (by `use` or by the prelude) yields `Quantity<N, U>`
   with the value unchanged. Two families in scope that define the same suffix is
   ORNA-S011. The qualified form `Family.name(n)` resolves the ambiguity and yields the
   same type as `n.name`.
2. A type `T` in scope with a literal target (P2 rule 2 of the kernel review: `T impl
   Currency`, or `T impl From<N>`) yields `T.from(n)`. This is library behaviour: for
   example `12.34.GBP` is `Money<GBP>`, and `1.months` is a `Period` (section 8.1).
3. Any other name is ORNA-UNIT-UNRESOLVED.

A unit suffix never converts an existing quantity: a quantity of key `U1` that receives
suffix `U2` is ORNA-UNIT-CONVERT. The suffix rule applies to numeric literals only.

**ORNA-UNIT-009 (family declarations).** A base family has exactly one bare variant, whose
key is its axis. A family with zero bare variants and at least one `=` variant is derived;
its dimension is computed from its variants. A family with two or more bare variants is
ORNA-UNIT-BASE. A cycle among variant definitions is ORNA-UNIT-CYCLE.

**ORNA-UNIT-010 (family membership).** Every variant `u` of family `F` has `dim(u) = V(F)`.
For a base family `V(F)` is the axis of its bare variant. For a derived family `V(F)` is the
common dimension of its variants. A mismatch is ORNA-UNIT-DIM, naming `F` and the printed
dimensions.

**ORNA-UNIT-011 (add, subtract, compare).** `Q_1 op Q_2` requires `V_1 = V_2`. Otherwise
ORNA-UNIT-DIM. The result unit is the finer operand: the key with the smaller `s`. The
other operand is converted by multiplying its value by `r = s_i / s_min`, which is at least
1. Equal keys give the same key. Compare follows the same conversion. A non-exact ratio
of `Decimal` operands is ORNA-UNIT-INEXACT.

**ORNA-UNIT-012 (multiply, divide).** The key of `Q_1 * Q_2` is `U_1 * U_2`; of `Q_1 / Q_2`
it is `U_1 / U_2`. Values multiply or divide by the numeric rule of `ops.rs`. Keys never
change a stored value.

**ORNA-UNIT-013 (integer power).** `Q ^ n` requires a literal `Int` exponent `n` (otherwise
ORNA-UNIT-POW). The key is `U ^ n`. `n = 0` gives the dimensionless key.

**ORNA-UNIT-014 (dimensionless fold).** If a result key has empty `V`, the result is a
plain numeric type, not a quantity. If its scale `s` is not 1, the value is multiplied by
`s`. An `Int` stays `Int` when `s` is an integer; otherwise the result is `Decimal` under
ORNA-UNIT-016. A `Decimal` result needs a finite decimal `s`, else ORNA-UNIT-INEXACT. A
`Float` result multiplies in IEEE binary.

**ORNA-UNIT-015 (explicit conversion, library).** `to(q, U)` is a library function, not a
typing rule (C7). It converts a quantity `q` of key `U1` to a key `U` of the same dimension:
the number is read with `erase`, multiplied by the exact ratio `s_1 / s_2` of section 5
item 3c, and rebuilt with `unerase<U>`. The result must be exact: an `Int` result with a
non-integer ratio widens to `Decimal` (ORNA-UNIT-016), and a `Decimal` result needs a finite
decimal ratio, else ORNA-UNIT-INEXACT. The rounded form `to(q, U, rounding)` takes a value
of the library enum `Rounding` (section 12, Q3) and is the only way to convert across an
inexact `Decimal` ratio. A suffix never converts: ORNA-UNIT-CONVERT. The name `to` is the
default (section 12, Q2).

**ORNA-UNIT-016 (numeric widening and exactness).** `Int` widens to `Decimal` exactly when a
conversion ratio is not an integer. `Float` never widens to `Decimal`. Any `Decimal`
conversion or fold whose ratio has no finite decimal form is ORNA-UNIT-INEXACT. Nothing
rounds implicitly.

**ORNA-UNIT-017 (currency): WITHDRAWN from the kernel (O4, decided).** A currency is not an
axis of a unit key. `Money<C>` is a library type (section 8.3) whose operations are library
impls. `Money<C> + Money<D>` for `C != D` has no impl, so it is an error, and the library
supplies the message that names both currencies (P7). A `Money<C>` divided by a unit
quantity gives a `Rate<C, U>`, and `Rate<C, U>` times a quantity of key `U` gives `Money<C>`.
`Money<GBP> * Money<GBP>` has no impl, so it is an error. MONEY-003 is a library impl and
ratio query, not a kernel rule. Wire tag 60007 is kept through the P9 registry.

**ORNA-UNIT-018 (canonical normalisation): WITHDRAWN (C5).** There is no `canonical`
clause. `1.hour` is `Quantity<Int, Duration.hour>` with value 1. A library may normalise
through a literal target, but the kernel does not. Consequence: `1.hour` and `3600.second`
have different keys with the same value, and they meet at the finer unit under
ORNA-UNIT-011. Section 12, Q4, records this as a default.

**ORNA-UNIT-019 (erasure and limits).** Units are static: runtime values are numeric values,
and every conversion factor is a compile-time constant. Unit variants are zero-size
singleton values (section 2). Where a value crosses a boundary, its schema type carries the
key. Exponents are in `[-32, 32]`. Rational numerators and denominators are bounded by the
evaluator's integer budget; overflow is ORNA-UNIT-LIMIT.

**ORNA-UNIT-020 (family annotation, new).** A unit family name `F` in a type position
denotes the nominal type of `F`'s quantities. A type `Quantity<N, U>` is accepted under `F`
when `U` is a variant key of `F`, for any `N`. A bare number, of any numeric type, is not
accepted under `F`: `let d: Duration = 5;` is ORNA-UNIT-ANNOT, and no conversion is
inserted (D7). The explicit forms are `Duration.second(5)` and `5.second`. A derived
product of variants (for example `second^2`) is not a variant of `Duration` and is
rejected. `F` as a type is nominal, not an alias of `Quantity<N, U>`. Realised by the
family record of section 11.1.

## 7. Worked examples (dogfooded)

Each example has valid and invalid lines. The invalid line produces the named diagnostic.
Values are illustrative; the rules decide the output type.

### 7.1 Length

```orna
pub enum Length {
    metre,
    kilometre = 1000 metre,
    mile = 1609.344 metre,
}

let a = 2.kilometre + 300.metre;      // finer metre (ORNA-UNIT-011): Quantity<Int, metre>, 2300
let b = 1.mile + 1.kilometre;         // finer kilometre; ratio 1.609344 is exact: Decimal 2.609344
let c = Length.kilometre(5);          // the same type and value as 5.kilometre
let bad = 3.metre + 2.second;         // ORNA-UNIT-DIM: Length and Duration
```

### 7.2 Speed (derived family)

```orna
pub enum Speed {
    kmh = Length.kilometre / Duration.hour,      // scale 1000/3600, that is 5/18
    mph = Length.mile / Duration.hour,           // scale 1609.344/3600
}

let v = 60.kmh + 10.mph;      // finer kmh; mph ratio exact: Decimal 76.09344 kmh
let bad = 60.kmh + 2.metre;   // ORNA-UNIT-DIM: Speed and Length
```

`kmh` has exponents `{metre: 1, second: -1}` and scale `5/18`. The expression
`Length.kilometre / Duration.hour` in a value gives the anonymous key, which prints as
`(5/18*metre/second)`.

### 7.3 Ambiguous suffix

```orna
pub enum Length { metre, kilometre = 1000 metre }
pub enum Storage { byte, kilobyte = 1000 byte }
pub enum Stream { chunk, kilometre = 3 chunk }    // a second family with the suffix kilometre

let ok = Length.kilometre(5);       // qualified form resolves the name
let bad = 5.kilometre;              // ORNA-S011: suffix kilometre is in two families in scope
```

### 7.4 Money and a rate (library)

Money and rates are library types (section 8.3). The currency is a type with the
`Currency` protocol. `12.50.GBP` is `Money<GBP>` by the literal-target rule (ORNA-UNIT-008,
step 2).

```orna
let sum = 12.50.GBP + 3.00.GBP;            // Money<GBP>, 15.50
let bad = 12.50.GBP + 3.00.USD;            // error: no impl for Money<GBP> + Money<USD> (P7 text)

let price = Rate<GBP, Energy.kilowatt_hour>.from(0.30);   // 0.30 GBP per kilowatt-hour
let cost = 3.kilowatt_hour * price;        // Money<GBP>, 0.90: library impl of section 8.3
let bad2 = price * price;                  // error: no impl Mul<Rate<GBP, _>, Rate<GBP, _>>
```

`Energy` must be in scope for `kilowatt_hour` (section 8.3). The check of MONEY-003 is the
library impl of `Mul` on `Rate<C, U>`: it converts the quantity to the rate's unit with `to`
(ORNA-UNIT-015) and multiplies the number. The kernel has no money rule.

### 7.5 Data size and a user rate

```orna
pub enum Data {
    byte,
    bit = 0.125 byte,
    kB = 1000 byte,
    KiB = 1024 byte,
}
pub enum DataRate {
    Mbps = 1000000 bit / Duration.second,
}

let total = 8.bit + 1.byte;          // finer bit; byte ratio 8 is an integer: Int 16 bit
let mixed = 1.KiB + 1.kB;            // finer kB; KiB ratio 1.024 exact: Decimal 2.024 kB
let bad = 1.byte + 1.second;         // ORNA-UNIT-DIM: Data and Duration
let bad2 = 1.Mbps + 1.byte;          // ORNA-UNIT-DIM: DataRate and Data
```

## 8. Listings: the embedded `core` module

These are the built-in declarations, written in full. They are ordinary Orna. A user module
may copy them verbatim, and the checker treats the copy the same way. The engine embeds
them as modules `core.time`, `core.data` and `core.money`.

### 8.1 Time

```orna
// core.time: elapsed time. Embedded in the engine; ordinary Orna.
pub enum Duration {
    second,
    s = second,
    seconds = second,
    min = 60 second,
    minute = 60 second,
    minutes = 60 second,
    hour = 3600 second,
    hours = 3600 second,
    day = 86400 second,
    days = 86400 second,
}
```

The names listed are the intrinsic duration suffixes of the 1.1.0 checker
(`lib.rs`, `INTRINSIC_DURATION_SUFFIXES`), plus `day` and `days`. The 1.1.0 checker read
`day` and `days` as calendar periods. Owner direction (Q1) makes them the elapsed 24-hour
`Duration`, so `now() - 7.days` means seven elapsed days.

Calendar arithmetic uses library names, not units. A calendar day varies with the time zone
(ORNA-TIME-004), so it is not an elapsed unit.

```orna
// core.time: calendar periods. Library; a Period is not a quantity of the algebra.
pub type Period { ... }                       // whole months and whole calendar days
impl Period {
    pub fn days(count: Int): Period = ...;    // the calendar day: Period.days(n)
    pub fn months(count: Int): Period = ...;
    pub fn years(count: Int): Period = ...;
}
// Literal targets (ORNA-UNIT-008, step 2): n.months and n.years are Period.months(n) and
// Period.years(n). There is no literal form for a calendar day: n.days is a Duration.
```

The body of `Period` is library-defined and is not restated here. The rules of this
section fix only its names and the rule that a calendar day is never `Duration`.

### 8.2 Data size

```orna
// core.data: data size. Embedded; ordinary Orna.
pub enum Data {
    byte,
    bit = 0.125 byte,
    kB = 1000 byte,
    MB = 1000000 byte,
    GB = 1000000000 byte,
    KiB = 1024 byte,
    MiB = 1048576 byte,
    GiB = 1073741824 byte,
}
```

### 8.3 Currency and money (library)

```orna
// core.money: currency and money. Library, not native. Wire tag 60007 is kept by the P9 registry.
pub protocol Currency {
    static code: Str;
    static minor_digits: Int;
}

/// An exact amount in currency `C`. The currency is in the type, not in the value.
pub type Money<C impl Currency> = Decimal {
    impl Add<Money<C>, Money<C>> {
        fn add(self, other: Money<C>): Money<C> = Money.from(self.base + other.base);
    }
    impl Sub<Money<C>, Money<C>> {
        fn sub(self, other: Money<C>): Money<C> = Money.from(self.base - other.base);
    }
    impl Mul<Decimal, Money<C>> {
        fn mul(self, factor: Decimal): Money<C> = Money.from(self.base * factor);
    }
    impl Explain {                   // P7: the words for `GBP + USD`
        static mismatch = "cannot $op $left and $right without a data-backed conversion";
    }
    impl Display {
        fn display(self, context: DisplayContext): Str = "{C.code} {self.base}";
    }
}

/// An amount of currency `C` per one unit `U`. Library: no kernel rule gives it meaning.
pub type Rate<C impl Currency, U> = Decimal {
    impl Mul<Quantity<Int, U>, Money<C>> {
        fn mul(self, amount: Quantity<Int, U>): Money<C> = Money.from(self.base * erase(amount));
    }
    impl Mul<Quantity<Decimal, U>, Money<C>> {
        fn mul(self, amount: Quantity<Decimal, U>): Money<C> = Money.from(self.base * erase(amount));
    }
}

// A currency is any type that implements `Currency`.
pub type GBP {
    impl Currency {
        static code = "GBP";
        static minor_digits = 2;
    }
}

// core.money: energy, used by the rate examples. Library example, not part of the native core.
pub enum Mass {
    kg,
}
pub enum Energy {
    J = Mass.kg * Length.metre^2 / Duration.second^2,
    kilowatt_hour = 3600000 J,
}
```

`Money<C>` is `Decimal` with impls, so `Money<GBP> * Money<GBP>` has no impl and is an
error (ORNA-UNIT-017). A quantity whose key is another unit of the same dimension is first
converted with `to` (ORNA-UNIT-015). The `Rate` impls read the number of a quantity only
through `erase`, which the kernel provides (section 5 item 3d).

### 8.4 Duration (nominal family)

`Duration` is the nominal name of the time family of section 8.1, not an alias (owner
direction, Q5). A value of type `Duration` is a quantity whose key is one of the family's
variants (ORNA-UNIT-020). Its wire form is the 1.1.0 tag 60005, `[seconds, nanosecond]`.
The engine binds that form to the `Duration` DefId through the P9 registry (section 12, Q7).
No clause is written on the declaration.

Library functions that return a time value return `Duration`. `Instant - Instant` gives a
`Duration`, and `Instant + Duration` gives an `Instant`. Conversion to and from other time
units is `to` (ORNA-UNIT-015). There is no rule that relates `Duration` to
`Quantity<Decimal, second>`; the 1.1.0 time view is withdrawn (O6).

### 8.5 Affine types (library)

Absolute temperatures are points and temperature differences are deltas. Both are library
types, so the kernel has no affine flag (ORNA-UNIT-005 withdrawn, C6).

```orna
// std/units/temperature.orna. ILLUSTRATION. Kelvin inside.
pub type Temperature {
    pub kelvin: Decimal,

    impl Sub<Temperature, Delta> {
        fn sub(self, other: Temperature): Delta = Delta { kelvin: self.kelvin - other.kelvin };
    }
    impl Add<Delta, Temperature> {
        fn add(self, change: Delta): Temperature = Temperature { kelvin: self.kelvin + change.kelvin };
    }
}

pub type Delta {
    pub kelvin: Decimal,

    impl Add<Delta, Delta> {
        fn add(self, other: Delta): Delta = Delta { kelvin: self.kelvin + other.kelvin };
    }
    impl Mul<Decimal, Delta> {
        fn mul(self, factor: Decimal): Delta = Delta { kelvin: self.kelvin * factor };
    }
    impl Add<Temperature, Temperature> {
        fn add(self, point: Temperature): Temperature = Temperature { kelvin: point.kelvin + self.kelvin };
    }
}
```

`Temperature + Temperature` has no impl, so it is an error. A Celsius literal such as
`20.celsius` is a library literal target (ORNA-UNIT-008, step 2) that adds 273.15. Its
aggregation is the library form of the 1.1.0 affine rows of ORNA-UNIT-007.

## 9. Options and trade-offs

**O1. Surface: enum-shaped families (decided, D15) versus keyword forms versus a marker
type.** The keyword forms `dim` and `unit` were the first draft. They added five reserved
words and a declaration kind the library cannot imitate. A marker type with an impl has no
unit list and no `case`. Enum-shaped families give a list that `case` and tools can walk,
one declaration form for built-in and user code, and the unit as a singleton variant. Cost:
the enum needs impl and static members (P3a) and singleton variants as type arguments (P3e);
a unit expression is a new production in enum bodies.

**O2. Exponent vector in the type (default) versus named derived families only.** Named-only
rejects `metre / second` in a value and forces a declared name for every expression.
Default keeps the vector. Trade-off: printing must be total (section 4).

**O3. Every unit has an exact scale (default).** A partial unit could not convert and would
need a run-time value. Trade-off: some physical constants need a declared scale.

**O4. Money as a library type (decided by owner direction).** `Money<C>` is a library type
(section 8.3) with a library `Rate<C, U>`. The kernel has no currency axis, so the rule
ORNA-UNIT-017 is withdrawn and `GBP * GBP` stays an error because no impl exists. The
kernel is one rule smaller. The cost is that `Money<GBP> / kWh` cancels through a library
`Rate` impl and a ratio query, not through the algebra. Both forms keep wire tag 60007.

**O5. Result unit of add: finer (default), left operand, or strict.** Finer never divides,
so conversion is exact whenever the ratio is an integer or a finite decimal. A left-operand
rule is order-dependent and loses precision. The strict rule (same key only) matches 1.1.0
literally but makes `1.kilometre + 1.metre` invalid. The finer rule is the 1.1.0
implementation's rule (`inference.rs` quantity sum), so it is a restatement, not a change.

**O6. `Duration` as the nominal name of the time family (decided by owner direction, Q5).**
The alternatives were an alias of `Quantity<Decimal, second>`, which the owner rejected
because a bare number could then flow in, and a separate primitive, which the owner
replaced with the family. The family keeps the wire form through the P9 registry and needs
no native primitive. Cost: the family-membership rule ORNA-UNIT-020 is one new kernel rule.

## 10. Migration from 1.1.0 and from the 1.x cut

### 10.1 What the old checker did

* `orna-check/src/units.rs` (1,503 lines): header collection, dimension and unit
  declarations, affine handling, the aggregation matrix and money rules.
* `orna-check/src/types.rs:38-51`: `Quantity { numeric, dimension_basis:
  BTreeMap<SourceDimensionIdentity, i64>, scale, offset, affine }` and `MoneyPerUnit {
  currency, unit }`. The exponent vector already existed.
* `orna-check/src/lib.rs` `infer_unit_suffix`: a suffix names a unit and never converts.
  Ambiguity is ORNA-S011.
* `orna-check/src/lib.rs`: the intrinsic duration suffixes and the calendar period suffixes
  (section 8.1).
* `orna-check/src/inference.rs:1168` `quantity_sum_type`: the finer-unit rule (`1.km + 1.m`
  is `1001.m`), which ORNA-UNIT-011 restates.

### 10.2 Mapping of the 1.1.0 clauses

| 1.1.0 clause | Status in this design | Realised by |
|---|---|---|
| ORNA-UNIT-001 compatibility | carried forward | key equality (3.3) |
| ORNA-UNIT-002 attached databases | carried forward | canonical bytes (4) |
| ORNA-UNIT-003 aggregation preserves dimension | carried forward | ORNA-UNIT-007, ORNA-UNIT-012 |
| ORNA-UNIT-004 incompatible addition | carried forward | ORNA-UNIT-011 |
| ORNA-UNIT-005 affine point and vector | withdrawn from kernel (C6) | library types (8.5) |
| ORNA-UNIT-006 integration | carried forward | ORNA-UNIT-006 |
| ORNA-UNIT-007 aggregation matrix | carried forward, non-affine part | ORNA-UNIT-007 |
| ORNA-UNIT-008 static suffix | carried forward with the P2 lookup | ORNA-UNIT-008 |
| ORNA-UNIT-015 conversion | library function `to` (C7) | ORNA-UNIT-015 |
| ORNA-UNIT-017 currency axis | withdrawn (O4) | `Money` and `Rate` (8.3) |
| ORNA-UNIT-018 canonical normalisation | withdrawn (C5) | none (ORNA-UNIT-018) |
| MONEY-001 to 007 | library behaviour | section 8.3; MONEY-003 is a library `Rate` impl |

### 10.3 What changes

| Old | New | Reason |
|---|---|---|
| `dimension_basis` keyed by source identity | exponent map over axes (canonical DefId) | HIR identity |
| `MoneyPerUnit` special case | `Money<C>` and `Rate<C, U>` as library types (O4) | one native rule fewer |
| Intrinsic duration suffixes in the checker | `Duration` family in `core.time` | dogfood |
| `day` and `days` as calendar periods | elapsed 24-hour `Duration`; calendar `Period` (Q1) | elapsed |
| `canonical` normalisation of `1.hour` to seconds | no normalisation; `1.hour` keeps its key (C5) | a visible change |
| `offset` and `affine` in the key | affine types as library (C6) | one key field fewer |
| `repr` and wire tags in declarations | engine registry keyed by DefId (P9) | no keyword |
| Add of mixed units | finer result (ORNA-UNIT-011) | explicit, exact |
| No explicit conversion | `to(q, U)` library function (ORNA-UNIT-015) | conversion is data |
| Dimension result is a quantity | dimensionless fold (ORNA-UNIT-014) | exact plain numeric |
| `dim` and `unit` keyword forms (first draft) | enum-shaped families (section 2, D15) | owner direction |

### 10.4 Migration for programs

* Programs without units are unaffected.
* Programs with `dim` or `unit` declarations are rejected by the 1.2 reference. They become
  valid with the enum-shaped forms of section 2. No program declares a unit today, so no
  migration of existing source is needed.
* `1.hour` keeps its key and prints as `hour`. Its value is 1, not 3600. Comparison with
  `3600.second` is exact (ORNA-UNIT-011). A column typed `Duration` keeps tag 60005.
* `90.days + 4.kilowatt_hour` stays invalid: ORNA-UNIT-DIM, naming `Duration` and `Energy`.
  `90.days` is an elapsed `Duration`; a calendar period is `Period.days(90)`.
* Mixed-scale add (`1.kilometre + 1.metre`) follows the finer-unit rule that the 1.1.0
  checker implemented in `quantity_sum_type` (`inference.rs:1168`). ORNA-UNIT-011 restates
  it; it is not a new superset.
* The sugar `Decimal<U>` of section 1 takes a unit named in scope, so a rate is written with
  the enum name, for example `Decimal<Energy.kilowatt_hour>`.

## 11. HIR implementation

### 11.1 Module and data types

* New module `crates/orna-hir/src/units.rs`, the one module. It owns `Rational`,
  `Exponents`, `UnitKey`, the key arithmetic, the rules of section 6, the printer of
  section 4 and the canonical bytes.
* `ty.rs`: `TyKind::Quantity { numeric: Ty, unit: UnitKeyId }`, with `UnitKeyId` an index
  into an interned unit-key table on `Program`. This replaces the removed `Quantity`
  placeholder and `MoneyPerUnit`. `Money` and `Rate` are ordinary library `TyKind::Named`
  types.
* `def.rs`: replace `DimensionDef { unit: Option<DefId> }` (line 247) with a family record
  `UnitFamilyDef { body: FamilyBody }`. `FamilyBody` is `Base { unit: DefId }` or `Derived`,
  the dimension being computed from the variants. Each unit variant is a
  `UnitDef { family: DefId, body: UnitBody }`; `UnitBody` is `Base` or `Derived { factor:
  Rational, terms: Vec<(DefId, i32)> }`. There is no offset and no affine flag.
* `typeck` gains the two casts `erase` and `unerase` (section 5 item 3d) and the family
  membership check of ORNA-UNIT-020.
* `lower/items.rs` builds these bodies at the sites that currently reference the unit
  fields (lines 56, 152, 157, 315, 458, 507). A post-lowering pass computes keys in
  dependency order.

### 11.2 Stage plan

* H2c-1, the kernel interface (section 5 items 1 to 6 and section 6): needs none of P1 to
  P3. It can land first. The unit families are lowered by the existing enum path with the
  unit body attached.
* H2c-2, the `core` listings (section 8): needs P1 (operator impls on `Money` and `Rate`),
  P2 (literal targets), P3a (impl and static members in enums and types), P3e (a
  payload-free variant as a type argument, for `Quantity<Decimal, Energy.kilowatt_hour>`),
  and P9 (the wire registry for tags 60005 and 60007). Parser prerequisite (P3a): an enum
  body accepts `impl` and `static` members, and the unit-variant production of section 2.
  This is a grammar change to `reference/01` (enum productions) and is named here, not made
  by this document.
* H2c-2 also removes `dim`, `unit`, `base`, `offset` and `affine` from the reserved list in
  `reference/01`, in the same change as the grammar productions. No keyword is added.
* Scope, with counts marked as estimates: new `orna-hir/src/units.rs` of about 900 to 1,200
  lines including rule tests; the `core` listings of section 8; changes to `def.rs`,
  `ty.rs`, `lower/items.rs`, `typeck/call.rs` (two sites) and `validate.rs` (one). The old
  `orna-check/src/units.rs` (1,503 lines) is not ported. Its cases are ported as oracle
  tests.
* The money and time parts of the checker (`units.rs` money parts, the `inference.rs` money
  rules, the intrinsic suffixes in `lib.rs`) are replaced by section 6 and section 8.
* H2c is rewritten, not ported. The earlier plan to delete the dimensional checks is
  cancelled by the D8 amendment. The 1.1.0 rules are the test oracle only.

## 12. Open questions and decisions

The kernel review (branch `docs/kernel-and-library`, PR #9856) records no answers to Q2 to
Q4, Q7 or the C-list at its head `afc6694f8`. Those items use the default below. The owner
may reverse any default. Each entry names its source.

* Q1. `day` and `days`. Decided by owner direction (2026-11-10): the elapsed 24-hour
  `Duration` (section 8.1). Calendar arithmetic uses `months`, `years` and `Period.days(n)`,
  never `day`. Reference `02-types.md` line 127 stands.
* Q2. Conversion name. Default: `to(q, U)`, a library function (C7). Owner may rename.
* Q3. Rounding conversion. Default: `to(q, U, rounding)` with a library enum `Rounding`, not
  a kernel rule (C7). Owner may reject the rounded form.
* Q4. Clause for `canonical`. Default (C5): no clause; ORNA-UNIT-018 is withdrawn. A side
  effect is that `1.hour` keeps its own key (section 6). Owner may reinstate the clause.
* Q5. `Duration`. Decided by owner direction: the nominal name of the time family, not an
  alias and not a separate primitive (section 8.4).
* Q6. Int under a `Duration` annotation. Decided by owner direction: an error (ORNA-UNIT-020),
  because D7 gives no implicit conversion. The explicit forms are `Duration.second(n)` and
  `n.second`.
* Q7. Wire forms of legacy-tagged types. Default (kernel review P9): an engine-side registry
  keyed by DefId, with no `repr` clause. Open: `core` paths then become ABI (kernel review
  Q4). Owner decides.
* O4. Money as a library type. Decided by owner direction (section 9).

## 13. Kernel review: what changed in this revision

The kernel review (branch `docs/kernel-and-library`, PR #9856; not in this tree) proposed
twelve changes. This revision:

* Applied: C1 (enum-shaped surface, section 2); C2 (`type` declaration for `Money`, not an
  alias, and no `alias` declaration); C3 (`Money<C impl Currency>`, already in the 1.1.0
  head); C4 (postfix lookup as the P2 rule, ORNA-UNIT-008); C5 (no `canonical` clause,
  ORNA-UNIT-018 withdrawn; default, Q4); C6 (affine types as library, ORNA-UNIT-005 withdrawn;
  default); C7 (`to` as a library function over `erase`, `unerase` and the ratio query;
  default, Q2 and Q3); C8 and O4 (`Money` and `Rate` as library types; owner direction); C9
  (the native minimum restated as the interface of section 5; the currency axis is removed
  from the kernel); C10 (H2c split into H2c-1 and H2c-2, section 11.2); C12 (pointer to P9
  for wire forms, Q7).
* Applied in part: C11 (`Duration`). The owner chose the nominal family (Q5), so there is no
  kernel integer. The time view is withdrawn; the family-membership rule ORNA-UNIT-020 is the
  one kernel addition.
* Not applied: none. C1 to C10 and C12 are applied as stated above. C11 is applied in part
  (the family form, not a kernel integer; Q5). Where the review recorded no decision, the
  defaults of section 12 apply.
