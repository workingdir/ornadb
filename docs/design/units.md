# Units, dimensions and dimensional algebra

Status: decided. AMENDED by owner direction 2026-11-10 (`docs/language/DECISIONS.md`,
D8 amendment). The surface is enum-shaped unit families, decided by owner direction
(D15, section 2). Section 12 sub-questions Q1 to Q6 are open, each with the default
this document uses. The document is a clean-sheet specification. It does not port the
1.1.0 implementation in `orna-check/src/units.rs`. It is the input for HIR stages
H2c-1 and H2c-2 (section 11.2), implemented in `orna-hir`.

Clause numbers: `ORNA-UNIT-001` to `008` are the 1.1.0 clauses, carried forward
with their 1.1.0 meaning. Section 6 says how each is realised. `ORNA-UNIT-009`
onward are new in 1.x.

Dogfooding principle. Built-in unit families and currency families are ordinary
Orna declarations, written in the same form a user writes. A reader who opens the
source of `Duration` or of a unit family reads plain Orna. The checker knows natively
only the minimum listed in section 5. Everything else is library Orna. Section 8
writes the embedded `core` declarations out in full.

## 1. Summary

* A **unit family** is an enum. A **unit** is a payload-free variant of a family.
* A **base family** has exactly one bare variant, the base unit. Its dimension is
  the axis of that unit. Other variants give scales: `kilometre = 1000 metre`.
* A **derived family** has no bare variant. Its variants are unit expressions, and
  its dimension is computed from them: `kmh = Length.kilometre / Time.hour`.
* A **dimension** is a name for an exponent vector over **base axes**. A base axis
  is a base unit or a nominal type implementing `Currency`.
* A **unit key** is an exponent vector, an exact rational scale, an exact rational
  offset and an affine flag. Two units with equal keys are the same unit for typing.
* A **quantity type** is `Quantity<N, U>`: a numeric type `N` in `{Int, Decimal,
  Float}` and a unit key `U`. The surface spellings `Decimal<kWh>` and `Float<kWh>`
  are sugar for `Quantity<Decimal, kWh>` and `Quantity<Float, kWh>`.
* Addition, subtraction and comparison need equal exponent vectors. Mixed units of
  one dimension meet at the finer unit (ORNA-UNIT-011).
* Multiplication, division and integer powers combine keys and never touch stored
  values.
* A dimensionless result is a plain numeric type. Its scale is folded into the
  value exactly, or it is a compile error (ORNA-UNIT-014).
* Money is a currency axis: `Money<C>` is `Quantity<Decimal, C>` (the alternative is O4).

## 2. Declarations (surface)

The surface is the enum form, decided by owner direction (D15). The keyword forms
`dim` and `unit` of the first draft of this document are withdrawn. Built-in and user
families take the same form.

```orna
pub enum Length {
    metre,                               // the base unit: the one bare variant
    kilometre = 1000 metre,
    mile = 1609.344 metre,
}

pub enum Time canonical second repr Duration {
    second,
    s = second,
    seconds = second,
    min = 60 second,
    minute = 60 second,
    minutes = 60 second,
    hour = 3600 second,
    hours = 3600 second,
}

pub enum Speed {                         // derived: no bare variant
    kmh = Length.kilometre / Time.hour,
    mph = Length.mile / Time.hour,
}

pub enum Temperature {
    kelvin,
    celsius = kelvin offset 273.15 affine,
}
```

Calls and literals, same for built-in and user families:

```orna
Length.kilometre(5)      // a Quantity<Int, kilometre>, the same as 5.kilometre
5.kilometre              // the suffix form (section 6, ORNA-UNIT-008)
```

Grammar. The new productions replace the `dim` and `unit` productions of the first
draft. Unchanged productions of the 1.2 grammar are not repeated.

```text
enum_declaration  := visibility 'enum' NAME generic_params? family_header*
                     '{' enum_body? '}'
family_header     := 'canonical' unit_name | 'repr' NAME
enum_body         := enum_variant (',' enum_variant)* ','?        (a unit family: unit variants only)
enum_variant      := contextual_name ( '{' payload_fields? '}' )?   (1.2 form, unchanged)
                   | contextual_name ( '=' unit_expr offset_clause? affine? )?   (unit variant)
unit_expr         := unit_term ( ('*' | '/')? unit_term )*        (left to right; '/' binds one term)
unit_term         := RATIONAL? unit_name ('^' INT)?
unit_name         := NAME | NAME '.' NAME                        (family-qualified variant)
offset_clause     := 'offset' RATIONAL      affine := 'affine'
```

Rules on the surface:

* A variant is either payload (1.2 enum) or unit (unit family). A family may not mix
  payload variants with unit variants.
* `RATIONAL` is an exact decimal literal such as `0.125`. Fractions are not written.
  A non-terminating ratio is not a valid scale.
* `unit_name` in a unit expression resolves by the static unit lookup of
  ORNA-UNIT-008. A bare name needs the variant in scope; `Family.variant` always
  resolves.
* `canonical U` names a variant of its family. It drives normalisation of postfix
  literals (ORNA-UNIT-018). Default: none. Kept pending C5 (section 13).
* `repr N` names a wire form from the 1.1.0 descriptor table (`29-formats.md`). It
  is the only native-facing attribute of this design. Section 5 lists it.
* `offset` and `affine` stay reserved words (section 2 of the reference, chapter 1).
  `dim`, `unit` and `base` are no longer needed as keywords; their reservation is
  lifted by the reference change that lands with H2c-2, not by this document.
* Rejected: a base family with zero or two bare variants (ORNA-UNIT-009); a derived
  variant with no `=`; a unit family whose variants do not share a dimension
  (ORNA-UNIT-010).

Unit variants are singleton types (P3e, option i). `Length.kilometre` is a value of
the enum type `Length` and a type `Quantity<Decimal, Length.kilometre>` names a unit.
Values of a unit family carry no run time data: the key is static (ORNA-UNIT-019).

## 3. Core model

### 3.1 Axes and exponent vectors

```text
Axis      := the canonical DefId of a base unit, or of a type implementing Currency
Exponents := finite map Axis -> Int, zero entries removed, each Int in [-32, 32]
```

Addition is pointwise; negation and integer scalar multiplication are pointwise.
The zero map is dimensionless.

### 3.2 Rational scale

Scales and offsets are exact rationals `p/q` over big integers, with `q > 0` and
`gcd(p, q) = 1`. A scale is positive. An offset may be zero or negative.

### 3.3 Unit key

```text
UnitKey := (V: Exponents, s: Rational > 0, o: Rational, a: bool)
```

* A base unit `metre` of axis `metre` has key `({metre: 1}, 1, 0, false)`.
* `kilometre = 1000 metre` has key `({metre: 1}, 1000, 0, false)`.
* `celsius = kelvin offset 273.15 affine` has key `({kelvin: 1}, 1, 273.15, true)`.
* `Temperature.kelvin` as a delta has key `({kelvin: 1}, 1, 0, false)`.
* A unit expression `u1 * u2^n / u3` has exponents that add with `n`; scales
  multiply with `s_i ^ n`; a literal factor multiplies the scale. A term with an
  affine key is an error (ORNA-UNIT-005). A non-affine key has offset 0.
* A unit key is a function of its expression only. Names are not in the key.

### 3.4 Quantity type

```text
Quantity<N, U>   N in {Int, Decimal, Float}, U a UnitKey interned per program
```

`Absolute<U>` is `Quantity<N, U>` with `U.a = true`. `Delta<U>` is `Quantity<N, U'>`
where `U'` is `U` with `o = 0` and `a = false`. So `Delta<C>` is the key of `kelvin`,
and `30.celsius - 20.celsius` is `10.kelvin`, printed as the delta unit of `celsius`.

### 3.5 Dimension of a key

`dim(U) = U.V`. The scale, offset and affine flag are ignored. Two keys are of the
same dimension when their exponent maps are equal. The dimension of a family is the
dimension of its variants (ORNA-UNIT-010).

## 4. Canonical form, printing, bytes

The canonical form of a quantity type is `(N, U)` with `U` as in 3.3. The type
carries the unit key, not a family name, so `Speed.kmh` and `Length.kilometre /
Time.hour` give one type.

Printing is total and deterministic:

1. If a declared unit has key `U`, print the name of the smallest canonical DefId
   among the candidates (the family-qualified variant name).
2. Otherwise, if `U.s = 1`, `U.o = 0` and `U.a = false`, print the axes in ascending
   canonical DefId order, positive exponents as numerator and negative as
   denominator, for example `metre/second` or `metre^2`.
3. Otherwise print `(S*` followed by (2) and `)`, where `S` is the scale as `p` or
   `p/q`.

A family name prints by the same rule. A base family prints its base unit.

Canonical bytes of a unit key, for attached-database equivalence (ORNA-UNIT-002):

```text
tag 0x55 | count:u32 | for each axis in ascending canonical name:
  name-len:u32 name-utf8 exponent:i32 | s.num s.den o.num o.den (signed decimal
  strings, big-endian length-prefixed) | a:u8
```

Stored values hold no unit bytes. A column's type carries the key.

## 5. Native minimum versus library Orna

The checker knows natively:

1. Exponent-map arithmetic and rational arithmetic (section 3).
2. The `Quantity<N, U>` type constructor, unit-key reduction of unit expressions,
   and dimension equality.
3. The four-item interface, kept under every option:
   a. `Quantity<N, U>` over `N` in `{Int, Decimal, Float}` and a key `U`;
   b. keys: an exponent map over base axes and an exact rational scale; key
      arithmetic for `*`, `/` and `^n` with an integer literal exponent;
   c. exactness and ratio queries: for two keys of one dimension, the finer key, and
      the exact ratio or "inexact", used by mixed-unit `+`, `-`, comparison and `to`;
   d. suffix resolution: the lookup of section 6, ORNA-UNIT-008, yields a key for a
      unit name.
4. The declaration pass for unit families: the variant scales, the dimension of each
   family (base or computed), and the `canonical` and `repr` header clauses. `repr`
   is checked against the wire-form table and nothing else.
5. Pending owner decision (flagged, C9 and O4): the nominal currency axis of
   ORNA-UNIT-017 stays native in this revision. The alternative, `Money<C>` as a library
   type with `Rate<C, U>`, is listed in O4.
6. The typing rules ORNA-UNIT-001 to ORNA-UNIT-019, in one module.

Everything else is library Orna: the time units, the size units, the currency family,
`Money<C>`, and the rate types written with them. Section 8 lists them as the
engine-embedded `core` module. A user family takes exactly the same form.

The checker does not know the names `hour`, `min`, `s`, `Duration`, `Money`, `GBP` or
`byte`. They are found by lookup like any other name. The one exception is the
calendar period, `day` and `days`, which is a built-in `Period` and not a unit
(ORNA-UNIT-008; open question Q1).

## 6. Typing rules

Notation: `Q_i = Quantity<N_i, U_i>`, `V_i = dim(U_i)`, and `r = s_1 / s_2` is the
scale ratio of two keys. An exact ratio is a rational whose decimal expansion
terminates.

**ORNA-UNIT-001 (compatibility, carried forward).** Two unit keys are compatible when
their reduced exponents, rational scale and affine behaviour agree. Names do not take
part. Realised by key equality (3.3).

**ORNA-UNIT-002 (structural equivalence, carried forward).** Compatible units from
attached databases interoperate when their structural definitions are equivalent.
Realised by the canonical bytes of section 4.

**ORNA-UNIT-003 (aggregation preserves dimension, carried forward).** An aggregate has
the dimension its algebra gives. Order statistics and sums keep the operand key;
products and quotients follow ORNA-UNIT-012. Realised by ORNA-UNIT-007 and
ORNA-UNIT-012.

**ORNA-UNIT-004 (incompatible addition, carried forward).** Adding, subtracting or
comparing quantities of different dimensions is a compile-time error where types are
known: `ORNA-UNIT-DIM`, naming both printed dimensions. Realised by ORNA-UNIT-011.

**ORNA-UNIT-005 (affine point and vector, carried forward).** For an affine unit `U`:

```text
Absolute<U> - Absolute<U> -> Delta<U>
Absolute<U> + Delta<U>    -> Absolute<U>
Delta<U>    + Absolute<U> -> Absolute<U>
Absolute<U> + Absolute<U> -> ORNA-UNIT-AFFINE
scalar * Absolute<U>      -> ORNA-UNIT-AFFINE       (also Absolute * Q and Q * Absolute)
scalar * Delta<U>         -> Delta<U>
Absolute<U> compare Delta<U> -> ORNA-UNIT-AFFINE
Absolute<U> op Absolute<V>, U != V -> ORNA-UNIT-AFFINE   (use `to`, ORNA-UNIT-015)
```

A delta operand combined with an absolute one is converted into the absolute unit by
the scale ratio of ORNA-UNIT-011. If that ratio is not exact, the result is
ORNA-UNIT-INEXACT. The result is absolute.

**ORNA-UNIT-006 (integration, carried forward).** Integration reduces dimensions
algebraically. Integrating speed over time gives a length: the key of the integrand
times the key of the variable.

**ORNA-UNIT-007 (aggregation matrix, carried forward).** For absolute affine values:

* selection and order operations (`first`, `last`, `min`, `max`, `median`, percentile
  and mode) are permitted and return an absolute value;
* `sum` is invalid for absolute affine values and valid for deltas;
* a non-empty `mean` is `base + mean(value - base)` for any selected base value in the
  input and returns an absolute value;
* `range` and standard deviation return the corresponding delta;
* variance returns the square of the corresponding delta;
* differences and derivatives operate on deltas;
* integration of an affine absolute quantity is invalid unless the caller first selects
  a meaningful linear reference.

**ORNA-UNIT-008 (static suffix resolution, carried forward).** Unit suffix resolution is
static and unambiguous in the current import context. A suffix `n.name` is in scope
when `name` is a variant of a unit family brought into scope by `use` or by the
prelude. Two families in scope that define the same suffix is ORNA-S011. The
qualified form `Family.name(n)` resolves the ambiguity and yields the same type as
`n.name`. A suffix never performs a `From` conversion. Postfix `value.name` with a
numeric `value` resolves `name` in this order: a unit in scope, which yields
`Quantity<N, U>` with the value unchanged; a currency, which yields
`Quantity<Decimal, C>`; a calendar period name (`day`, `days`), which yields the
built-in `Period`. Any other name is ORNA-UNIT-UNRESOLVED.

**ORNA-UNIT-009 (family declarations).** A base family has exactly one bare variant,
whose key is its axis. A family with zero bare variants and at least one `=` variant
is derived; its dimension is computed from its variants. A family with two or more
bare variants is ORNA-UNIT-BASE. A cycle among variant definitions is ORNA-UNIT-CYCLE.
`canonical U` and `repr N` must name a variant of the family and a wire form of the
1.1.0 table, respectively.

**ORNA-UNIT-010 (family membership).** Every variant `u` of family `F` has
`dim(u) = V(F)`. For a base family `V(F)` is the axis of its bare variant. For a
derived family `V(F)` is the common dimension of its variants. A mismatch is
ORNA-UNIT-DIM, naming `F` and the printed dimensions.

**ORNA-UNIT-011 (add, subtract, compare).** `Q_1 op Q_2` requires `V_1 = V_2`. Otherwise
ORNA-UNIT-DIM. For non-affine operands the result unit is the finer operand: the key
with the smaller `s`. The other operand is converted by multiplying its value by
`r = s_i / s_min`, which is at least 1. Equal keys give the same key. For a delta and
an absolute operand, see ORNA-UNIT-005. Compare follows the same conversion.

**ORNA-UNIT-012 (multiply, divide).** The key of `Q_1 * Q_2` is `U_1 * U_2`; of
`Q_1 / Q_2` it is `U_1 / U_2`. Values multiply or divide by the numeric rule of
`ops.rs`. Keys never change a stored value. Affine operands are rejected
(ORNA-UNIT-005). Integration and derivatives use this rule (ORNA-UNIT-006).

**ORNA-UNIT-013 (integer power).** `Q ^ n` requires a literal `Int` exponent `n`
(otherwise ORNA-UNIT-POW). The key is `U ^ n`. Affine operands are rejected. `n = 0`
gives the dimensionless key.

**ORNA-UNIT-014 (dimensionless fold).** If a result key has empty `V`, the result is a
plain numeric type, not a quantity. If its scale `s` is not 1, the value is multiplied
by `s`. An `Int` stays `Int` when `s` is an integer; otherwise the result is `Decimal`
under ORNA-UNIT-016. A `Decimal` result needs a finite decimal `s`, else
ORNA-UNIT-INEXACT. A `Float` result multiplies in IEEE binary.

**ORNA-UNIT-015 (explicit conversion).** `q.to(U)` converts to a unit `U` of the same
dimension. For absolute operands the value `v` in a source with scale `s_src` and
offset `o_src` becomes `(v * s_src + o_src - o_dst) / s_dst`. For deltas it becomes
`v * s_src / s_dst`. The result must be exact: an `Int` result with a non-integer ratio
widens to `Decimal` (ORNA-UNIT-016), and a `Decimal` result needs a finite decimal
ratio, else ORNA-UNIT-INEXACT. A suffix never converts; a quantity of key `U1` that
receives suffix `U2` is ORNA-UNIT-CONVERT. The name `to` is the one new surface name
(open question Q2).

**ORNA-UNIT-016 (numeric widening and exactness).** `Int` widens to `Decimal` exactly
when a conversion ratio is not an integer. `Float` never widens to `Decimal`. A key
with a currency axis rejects `Float` (ORNA-UNIT-NUMERIC) and accepts `Int` through
`Decimal`. Any `Decimal` conversion or fold whose ratio has no finite decimal form is
ORNA-UNIT-INEXACT. Nothing rounds implicitly; an explicit rounding function is library
Orna (open question Q3).

**ORNA-UNIT-017 (currency axis).** A nominal type implementing `Currency` is a base
axis. `Money<C>` is `Quantity<Decimal, C>`. Adding `Money<C>` and `Money<D>` for
`C != D` is ORNA-UNIT-DIM, naming both currencies. Currency conversion is never
implicit; it is a named, data-backed operation of the library (MONEY-005). A key with
two currency axes is rejected (option O4).

**ORNA-UNIT-018 (canonical normalisation).** A family declared `canonical U` makes a
postfix literal on any of its variants normalise to `U` exactly: `1.hour` yields
`Quantity<Int, second>` with value 3600. Inexact normalisation is ORNA-UNIT-INEXACT.
Only the time family declares a canonical unit (open question Q4). Length does not, so
`1.kilometre` keeps the `kilometre` key.

**ORNA-UNIT-019 (erasure and limits).** Units are static: runtime values are numeric
values, and every conversion factor is a compile-time constant. Unit variants are
zero-size singleton values (section 2). Where a value crosses a boundary, its schema
type carries the key. Exponents are in `[-32, 32]`. Rational numerators and
denominators are bounded by the evaluator's integer budget; overflow is ORNA-UNIT-LIMIT.

## 7. Worked examples (dogfooded)

Each example has valid and invalid lines. The invalid line produces the named
diagnostic. Values are illustrative; the rules decide the output type.

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
let bad = 3.metre + 2.second;         // ORNA-UNIT-DIM: Length and Time
```

### 7.2 Speed (derived family)

```orna
pub enum Speed {
    kmh = Length.kilometre / Time.hour,      // scale 1000/3600, that is 5/18
    mph = Length.mile / Time.hour,           // scale 1609.344/3600
}

let v = 60.kmh + 10.mph;      // finer kmh; mph ratio exact: Decimal 76.09344 kmh
let bad = 60.kmh + 2.metre;   // ORNA-UNIT-DIM: Speed and Length
```

`kmh` has exponents `{metre: 1, second: -1}` and scale `5/18`. The expression
`Length.kilometre / Time.hour` in a value gives the anonymous key, which prints as
`(5/18*metre/second)`.

### 7.3 Ambiguous suffix

```orna
pub enum Length { metre, kilometre = 1000 metre }
pub enum Storage { byte, kilobyte = 1000 byte }
pub enum Stream { chunk, kilometre = 3 chunk }    // a second family with the suffix kilometre

let ok = Length.kilometre(5);       // qualified form resolves the name
let bad = 5.kilometre;              // ORNA-S011: suffix kilometre is in two families in scope
```

### 7.4 Currency family

```orna
pub type GBP { impl Currency { static code = "GBP"; static minor_digits = 2; } }
pub type USD { impl Currency { static code = "USD"; static minor_digits = 2; } }

let sum = 12.50.GBP + 3.00.GBP;          // Quantity<Decimal, GBP>, that is Money<GBP>
let bad = 12.50.GBP + 3.00.USD;          // ORNA-UNIT-DIM: Money<GBP> and Money<USD>
```

With the `Energy` family of section 8.3 in scope:

```orna
let cost = 3.kilowatt_hour * (1.00.GBP / 1.kilowatt_hour);   // Money<GBP>: energy cancels (MONEY-003)
```

Check: `kWh` has key `({kg: 1, metre: 2, second: -2}, 3600000, 0, false)`. `GBP / kWh`
has key `({GBP: 1, kg: -1, metre: -2, second: 2}, 1/3600000, 0, false)`. The product
has `V = {GBP: 1}` and scale `1`, so the result is `Money<GBP>`. MONEY-003 is therefore
a consequence of ORNA-UNIT-012 and ORNA-UNIT-014, not a special rule.

### 7.5 Data size and a user rate

```orna
pub enum Data {
    byte,
    bit = 0.125 byte,
    kB = 1000 byte,
    KiB = 1024 byte,
}
pub enum DataRate {
    Mbps = 1000000 bit / Time.second,
}

let total = 8.bit + 1.byte;          // finer bit; byte ratio 8 is an integer: Int 16 bit
let mixed = 1.KiB + 1.kB;            // finer kB; KiB ratio 1.024 exact: Decimal 2.024 kB
let bad = 1.byte + 1.second;         // ORNA-UNIT-DIM: Data and Time
let bad2 = 1.Mbps + 1.byte;          // ORNA-UNIT-DIM: DataRate and Data
```

## 8. Listings: the embedded `core` module

These are the built-in declarations, written in full. They are ordinary Orna. A user
module may copy them verbatim, and the checker treats the copy the same way. The
engine embeds them as module `core.units`.

### 8.1 Time

```orna
// core.units: time. Embedded in the engine; ordinary Orna.
pub enum Time canonical second repr Duration {
    second,
    s = second,
    seconds = second,
    min = 60 second,
    minute = 60 second,
    minutes = 60 second,
    hour = 3600 second,
    hours = 3600 second,
}
```

The names listed are exactly the intrinsic duration suffixes of the 1.1.0 checker
(`lib.rs`, `INTRINSIC_DURATION_SUFFIXES`). `day` and `days` are not declared. A calendar
day varies with the time zone (ORNA-TIME-004), so it is the built-in `Period`
(ORNA-UNIT-008), not a unit of the algebra.

### 8.2 Data size

```orna
// core.units: data size. Embedded; ordinary Orna.
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

### 8.3 Currency family and energy

```orna
// core.units: currency family. Embedded; ordinary Orna.
pub protocol Currency {
    static code: Str;
    static minor_digits: Int;
}
pub type Money<C impl Currency> = Quantity<Decimal, C>;   // wire tag 60007, bound by P9

// core.units: energy, used by the rate examples. Library, not core.
pub enum Mass {
    kg,
}
pub enum Energy {
    J = kg metre^2 / second^2,
    kilowatt_hour = 3600000 J,
}
```

`Money` uses the 60007 codec. Its wire form is set by the P9 registry, keyed on the
`Money` DefId, and no clause is written on the declaration (flagged: this replaces the
`repr Money` alias clause of the first draft, which the reference's type grammar does
not admit). The examples in section 7.4 need `Energy` in scope; `kilowatt_hour` is
named here to keep the example readable. The energy family is a library example, not
part of the native core.

### 8.4 Duration

`Duration` is the 1.1.0 primitive (`29-formats.md`, tag 60005: `[seconds, nanosecond]`).
It is declared in the embedded core as a body-less native type, bound by DefId (D12).
The `repr Duration` clause on the time family says that the time quantity has this
wire form. The checker's view of a Duration value is `Quantity<Decimal, second>`, and
the conversion between the two is the codec of 60005, not a rule of the algebra.
Whether the nominal primitive is a separate type or only a view of the time quantity
is open question Q5.

## 9. Options and trade-offs

**O1. Surface: enum-shaped families (decided, D15) versus keyword forms versus a marker
type.** The keyword forms `dim` and `unit` were the first draft. They added five
reserved words and a declaration kind the library cannot imitate. A marker type with an
impl has no unit list and no `case`. Enum-shaped families give a list that `case` and
tools can walk, one declaration form for built-in and user code, and the unit as a
singleton variant. Cost: the enum needs impl and static members (P3a) and singleton
variants as type arguments (P3e); a unit expression is a new production in enum bodies.

**O2. Exponent vector in the type (default) versus named derived families only.**
Named-only rejects `metre / second` in a value and forces a declared name for every
expression. Default keeps the vector. Trade-off: printing must be total (section 4).

**O3. Every unit has an exact scale (default).** A partial unit could not convert and
would need a runtime value. Trade-off: some physical constants need a declared scale.

**O4. Money as a currency axis (default) versus `Money<C>` as a library type with
`Rate<C, U>`.** The axis form makes `Money<GBP> / kWh` cancel with `Decimal<kWh>` by
ORNA-UNIT-012, with no special rule. Trade-off: a currency is a base axis, so a product
of two currencies is a valid exponent vector. Default: reject a key with two currency
axes (ORNA-UNIT-017). Alternative: `Money<C>` is a library type (listing 4 of the kernel
review) and a library `Rate<C, U>` replaces the cancellation. The alternative removes
one native rule. MONEY-003 is then a `Rate` impl plus the ratio query for `kWh`
against `J`, and `Money<GBP> * Money<GBP>` is an error because no impl exists. Both keep
wire tag 60007. The owner decides.

**O5. Result unit of add: finer (default), left operand, or strict.** Finer never
divides, so conversion is exact whenever the ratio is an integer or a finite decimal. A
left-operand rule is order-dependent and loses precision. The strict rule (same key
only) matches 1.1.0 literally but makes `1.kilometre + 1.metre` invalid. The finer rule
is the 1.1.0 implementation's rule (`inference.rs` quantity sum), so it is a
restatement, not a change.

**O6. `Duration` as the primitive with a time-dimension view (default) versus an alias
of `Quantity<Decimal, second>`.** An alias would contradict the 1.1.0 primitive list and
the 60005 tag. The primitive keeps the wire form; `repr` ties the time family to it.
Cost: the checker must know that `Duration` values are the time view (section 5, item
4). Recommendation: the primitive, as above.

## 10. Migration from 1.1.0 and from the 1.x cut

### 10.1 What the old checker did

* `orna-check/src/units.rs` (1,503 lines): header collection, dimension and unit
  declarations, affine handling, the aggregation matrix and money rules.
* `orna-check/src/types.rs:38-51`: `Quantity { numeric, dimension_basis:
  BTreeMap<SourceDimensionIdentity, i64>, scale, offset, affine }` and `MoneyPerUnit {
  currency, unit }`. The exponent vector already existed.
* `orna-check/src/lib.rs` `infer_unit_suffix`: a suffix names a unit and never converts.
  Ambiguity is ORNA-S011.
* `orna-check/src/lib.rs`: the intrinsic duration suffixes and the calendar period
  suffixes (section 8.1).
* `orna-check/src/inference.rs:1168` `quantity_sum_type`: the finer-unit rule
  (`1.km + 1.m` is `1001.m`), which ORNA-UNIT-011 restates.

### 10.2 Mapping of the 1.1.0 clauses

| 1.1.0 clause | Status in this design | Realised by |
|---|---|---|
| ORNA-UNIT-001 compatibility | carried forward | key equality (3.3) |
| ORNA-UNIT-002 attached databases | carried forward | canonical bytes (4) |
| ORNA-UNIT-003 aggregation preserves dimension | carried forward | ORNA-UNIT-007, ORNA-UNIT-012 |
| ORNA-UNIT-004 incompatible addition | carried forward | ORNA-UNIT-011 |
| ORNA-UNIT-005 affine point and vector | carried forward | 3.4, ORNA-UNIT-005 |
| ORNA-UNIT-006 integration | carried forward | ORNA-UNIT-006 |
| ORNA-UNIT-007 aggregation matrix | carried forward | ORNA-UNIT-007 |
| ORNA-UNIT-008 static suffix | carried forward | ORNA-UNIT-008 |
| MONEY-001 to 007 | carried forward | ORNA-UNIT-017; MONEY-003 is ORNA-UNIT-012 with ORNA-UNIT-014 |

### 10.3 What changes

| Old | New | Reason |
|---|---|---|
| `dimension_basis` keyed by source identity | exponent map over axes (canonical DefId) | HIR identity |
| `MoneyPerUnit` special case | currency as an axis (ORNA-UNIT-017) | one rule |
| Intrinsic duration suffixes in the checker | `core.units` enum families and `canonical` | dogfood |
| Add of mixed units | finer result (ORNA-UNIT-011) | explicit, exact |
| No explicit conversion | `to(U)` (ORNA-UNIT-015) | the one new name |
| Dimension result is a quantity | dimensionless fold (ORNA-UNIT-014) | exact plain numeric |
| `dim` and `unit` keyword forms (first draft) | enum-shaped families (section 2, D15) | owner direction |

### 10.4 Migration for programs

* Programs without units are unaffected.
* Programs with `dim` or `unit` declarations are rejected by the 1.2 reference. They
  become valid with the enum-shaped forms of section 2. No program declares a unit
  today, so no migration of existing source is needed.
* `Float<kWh>` and `3.kWh` keep their meaning (ORNA-UNIT-008). The name `kWh` is
  written `kilowatt_hour` in section 8.3 of this document; the built-in unit names are
  the ones section 8 declares.
* `90.days + 4.kilowatt_hour` stays invalid: ORNA-UNIT-DIM, naming the calendar `Period`
  and `Energy`.
* Mixed-scale add (`1.kilometre + 1.metre`) follows the finer-unit rule that the 1.1.0
  checker already implemented in `quantity_sum_type` (`inference.rs:1168`). ORNA-UNIT-011
  restates it; it is not a new superset.

## 11. HIR implementation

### 11.1 Module and data types

* New module `crates/orna-hir/src/units.rs`, the one module. It owns `Rational`,
  `Exponents`, `UnitKey`, the key arithmetic, the rules of section 6, the printer of
  section 4 and the canonical bytes.
* `ty.rs`: `TyKind::Quantity { numeric: Ty, unit: UnitKeyId }`, with `UnitKeyId` an index
  into an interned unit-key table on `Program`. This replaces the removed `Quantity`
  placeholder and `MoneyPerUnit`.
* `def.rs`: replace `DimensionDef { unit: Option<DefId> }` (line 247) with a family
  record `UnitFamilyDef { body: FamilyBody, canonical: Option<DefId>, repr: Option<Repr> }`.
  `FamilyBody` is `Base { unit: DefId }` or `Derived`, the dimension being computed from
  the variants. Each unit variant is a `UnitDef { family: DefId, body: UnitBody }`;
  `UnitBody` is `Base` or `Derived { factor: Rational, terms: Vec<(DefId, i32)>,
  offset: Rational, affine: bool }`.
* `lower/items.rs` builds these bodies at the sites that currently reference the unit
  fields (lines 56, 152, 157, 315, 458, 507). A post-lowering pass computes keys in
  dependency order.

### 11.2 Stage plan

* H2c-1, the kernel interface (section 5 items 1 to 4 and section 6): needs none of P1
  to P3. It can land first. The unit families are lowered by the existing enum path
  with the unit body attached.
* H2c-2, the `core.units` listings (section 8): needs P2 (suffix rule), P3a (impl and
  static members in enums, prerequisite for the Suffix and `case` forms), and P3e
  (a payload-free variant as a type argument, for `Quantity<Decimal, Length.kilometre>`).
  Parser prerequisite (P3a): an enum body accepts `impl` and `static` members, and the
  unit-variant production of section 2. This is a grammar change to `reference/01`
  (enum productions) and is named here, not made by this document.
* H2c-2 also removes `dim`, `unit` and `base` from the reserved list in
  `reference/01`, in the same change as the grammar productions.
* Scope, with counts marked as estimates: new `orna-hir/src/units.rs` of about 900 to
  1,200 lines including rule tests; the `core.units` listings of section 8; changes to
  `def.rs`, `ty.rs`, `lower/items.rs`, `typeck/call.rs` (two sites), `validate.rs` (one)
  and `classify.rs` (one). The old `orna-check/src/units.rs` (1,503 lines) is not
  ported. Its cases are ported as oracle tests.
* The money and time parts of the checker (`units.rs` money parts, the `inference.rs`
  money rules, the intrinsic suffixes in `lib.rs`) are replaced by section 6.
* H2c is rewritten, not ported. The earlier plan to delete the dimensional checks is
  cancelled by the D8 amendment. The 1.1.0 rules are the test oracle only.

## 12. Open questions

Each sub-question below is open for the owner and has the default this design uses.

* Q1. `day` and `days`. The reference `02-types.md` says `1.day` is an elapsed
  `Duration`; the checker and ORNA-TIME-004 say a calendar `Period`. This design follows
  the checker. The owner must decide, and the reference corrected if needed.
* Q2. Explicit conversion name. `q.to(U)` is proposed. Owner to confirm.
* Q3. Rounding conversion, `to(U, scale, rounding)`, for `Decimal` non-finite ratios.
  Proposed as a library function, not a rule. Owner to confirm.
* Q4. Whether `canonical` is a general family clause or only for time. Default: general
  clause; only time declares one.
* Q5. Is `Duration` a separate nominal primitive, converted to and from the time
  quantity by the 60005 codec, or only a view of `Quantity<Decimal, second>`? Recommended:
  separate primitive (O6). Owner to decide.
* Q6. Does a literal `Int` quantity of time (for example `1.hour + 30.minute`, `Int`
  5400 second) fit a `Duration` annotation, given that ORNA-UNIT-016 widens `Int` only
  for non-integer ratios? This design does not add a widening rule for annotations. The
  example in section 7 carries no annotation for that reason. Owner to decide.
* Q7. `repr` for legacy-tagged types. The wire-clause decision for all legacy-tagged
  types is the P9 question of the kernel review (branch `docs/kernel-and-library`, not in
  this tree, section 4.9). This document uses `repr` on the time family and the P9 registry for
  `Money` and only points to P9.

## 13. Kernel review: what changed in this revision

The kernel review (branch `docs/kernel-and-library`, not in this tree, section 10) proposed
twelve changes. This revision:

* Applied: C1 (enum-shaped surface, section 2), C2 (`type` declaration for `Money`, not
  an alias), C3 (`Money<C impl Currency>`, already in the 1.1.0 head), C8 (the
  `Money<C>` library alternative added to O4), C9 (the native minimum restated as the
  four-item interface, section 5; currency axis kept native and flagged), C10 (H2c split
  into H2c-1 and H2c-2, section 11.2), C12 (pointer to P9 for `repr`, Q7).
* Not applied, pending the owner: C4 (postfix lookup as the P2 rule), C5 (`canonical`
  as library normalisation), C6 (affine units as library types), C7 (`to` as a library
  function), C11 (`Duration` as a library type over a kernel integer). Each keeps the
  1.2 behaviour of this document until the owner answers.
