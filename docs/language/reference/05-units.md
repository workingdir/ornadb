# 5. Units

A unit gives a numeric value a dimension. Length, time and data size are the
typical uses. A unit is a variant of a unit family, and the checker refuses to add
quantities of different dimensions. This chapter is the user-facing rule set. The
full specification is `docs/design/units.md`.

## Declaring a unit family

A unit family is an enum, followed by the marker `impl Unit {}` as its last line. Its variants are units:

* A **base family** has exactly one bare variant, the base unit. Other variants
  give scales of it, written `name = factor base`.
* A **derived family** has no bare variant. Each variant is a product or quotient of
  other units, written `name = a / b`.

```orna
pub enum Length {
    metre,
    kilometre = 1000 metre,
    mile = 1609.344 metre,
}
impl Unit {}

pub enum Speed {
    kmh = Length.kilometre / Duration.hour,
    mph = Length.mile / Duration.hour,
}
impl Unit {}
```

Built-in families (`Duration`, and the data-size family) are declared the same way. A user family
takes the same form. An enum without the marker is an ordinary enum, and a `=` variant in it is an
error.

## Using a unit

A unit variant is written as a suffix on a numeric literal, or as a call:

```orna
let a = 5.kilometre;            // Quantity<Int, Length.kilometre>
let b = Length.kilometre(5);    // the same type and value
```

A family name used as a type, such as `let d: Duration = 5;`, accepts units of that family and no
bare number. A quantity of another unit in the family is converted to the family's base unit at that
position, so `let d: Duration = 1.hour;` stores 3600 seconds.

A suffix names a unit in scope. It never converts an existing quantity. If two
families in scope define the same suffix, the suffix is ambiguous; use the qualified
call form `Family.name(n)` to choose one.

A quantity type is `Quantity<N, U>`, where `N` is `Int`, `Decimal` or `Float` and `U`
is the unit. `Decimal<U>` and `Float<U>` are shorthand for `Quantity<Decimal, U>` and
`Quantity<Float, U>`.

## Arithmetic

* Adding, subtracting or comparing quantities needs the same dimension. The result
  is in the finer of the two units. The other operand is converted by an exact ratio;
  a `Decimal` ratio with no finite decimal form is `ORNA-UNIT-INEXACT`.
* Multiplying and dividing combine units. `Length.metre / Duration.second` is a
  speed, and a product that cancels to no dimension is a plain number.
* Converting to another unit of the same dimension is the library function
  `to(q, U)`. It is exact, or it is an error. The rounded form takes a `Rounding` value.

## Errors

| Diagnostic | When |
|---|---|
| `ORNA-UNIT-DIM` | quantities of different dimensions are added, subtracted or compared |
| `ORNA-UNIT-020` | a bare number is given a unit-family type, such as `let d: Duration = 5;` |
| `ORNA-UNIT-CONVERT` | a quantity is given a second unit by a suffix |
| `ORNA-UNIT-INEXACT` | a `Decimal` conversion or result has no finite decimal form |
| `ORNA-S011` | a suffix names a unit in two families in scope |

A bare number is never converted to a unit. Write `Duration.second(n)` or `n.second`.

## Time and money

`Duration` is the elapsed time family. `1.day` and `7.days` are 24-hour durations.
`day` and `days` are never calendar periods. Calendar arithmetic uses `Period`:
`1.months`, `1.years` and `Period.days(n)`.

`Money<C>` and the rate type `Rate<C, U>` are library types, not kernel units. Adding
two different currencies, or multiplying two `Money<GBP>` values, is a type error.

## Worked examples

### Lengths

```orna
let total = 2.kilometre + 300.metre;   // finer unit: 2300 metre
let bad = 3.metre + 2.second;          // ORNA-UNIT-DIM: Length and Duration
```

### A derived speed

```orna
let v = 60.kmh + 10.mph;               // finer unit kmh; 76.09344 kmh
let bad = 60.kmh + 2.metre;            // ORNA-UNIT-DIM: Speed and Length
```

### Durations and calendar periods

```orna
let wait: Duration = 90.days;          // 90 elapsed 24-hour days
let shift: Duration = 2.hour + 30.min; // finer unit min, value 150 min
let bad: Duration = 90;                // ORNA-UNIT-020: write 90.second or Duration.second(90)
let month = 1.months;                  // a Period, not a Duration
```

## Decided by

D8 (units as enum families, amended 2026-11-10), with the owner decisions of the same
date: `day` and `days` are elapsed `Duration` (Q1); `Duration` is the nominal time
family (Q5); an `Int` does not flow into a `Duration` annotation (Q6); `Money` is a
library type (O4).
