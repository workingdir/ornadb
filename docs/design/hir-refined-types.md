# Refined-type base access (P3c)

Status: proposed. Owner decision needed before implementation. This document changes no code and no spec text.

Source: [kernel-and-library.md](kernel-and-library.md), section P3c (lines 549-558) and gap G5 (line 208).

## Problem

A refined nominal type is represented by its base and is distinct from it (`reference/Orna-1.1.0/source/05-types.md`, ORNA-NOMINAL-003). Construction runs every assertion (ORNA-REFINE-001). A refined value has no way to read its base from the type's own nested implementations. The checker offers only `Target.from(value)` for construction (G5). Library code that formats or computes on the representation cannot be written.

The spec's own example depends on this. `reference/Orna-1.1.0/source/13-presentation.md:74-79` defines `FriendlyDuration = Duration { impl Display { ... compact(self, context) } }`. ORNA-NOMINAL-003 makes `FriendlyDuration` distinct from `Duration`, and the spec does not say which rule lets `self` reach a `Duration` parameter. The example relies on a representation read that the spec does not define.

## Constraints from the spec

- ORNA-NOMINAL-006: private fields are visible to the owning type's nested implementations and to no unrelated module.
- ORNA-NOMINAL-007: outside construction uses an exposed conversion or method.
- ORNA-NOMINAL-008: field visibility controls representation access, not mutability.
- ORNA-CONVERT-001: `Target.from(source)` selects a `From<Source>` implementation nested in `Target`. The target owns its conversions.
- ORNA-CONVERT-006: arbitrary `From` implementations are not applied implicitly.

## Options

### i. Implicit private `base` (recommended)

A refined type exposes one read-only member `base` of the base type. It is visible in the type's nested implementations and nowhere else, the same rule as a private field under ORNA-NOMINAL-006.

- Buys: follows the existing privacy rule. No public surface. No new conversion. ORNA-CONVERT-001 is unchanged.
- Costs: a new member name. `kernel-and-library.md` (line 555) notes that `base` is already a keyword allowed after `.`, so it must be reserved or resolved deliberately. The spec needs a clause for it. Readers can use it only inside nested implementations, so callers cannot depend on the representation.

### ii. Built-in conversion out of a refined type (`Int.from(p)`)

- Buys: no new member.
- Costs: ORNA-CONVERT-001 says the target owns its `From` implementations. `Int` owns none, because built-in types have no impl blocks. This needs a checker special case that bypasses the ownership rule. It also covers only conversion to the direct base, not general access from a nested implementation.

### iii. Public `base` on `pub` refined types

- Buys: simplest. Little checker code.
- Costs: exposes the representation of every `pub` refined type to other modules. That weakens the nominal distinction ORNA-NOMINAL-003 and ORNA-NOMINAL-007 protect, and callers can then depend on the representation.

## Proposal

Adopt option i. Add a read-only `base` to refined types, readable only in nested implementations under the ORNA-NOMINAL-006 rule.

Also reconcile `13-presentation.md:74-79`. Either the example writes `self.base`, or the spec adds the conversion rule that makes the call legal. Both are spec changes to `reference/Orna-1.1.0`, so the owner approves them first.

Acceptance criteria for the implementation PR (not this one):

- A nested implementation of a refined type reads `self.base` and gets the base value.
- A read of `base` from an unrelated module is rejected with the ORNA-NOMINAL-006 visibility diagnostic.
- Stored form is unchanged. Tag 60020 stays `[type_object_id, base_value]` (`reference/Orna-1.1.0/source/29-formats.md`).
- Phantom-parameter access (`Money<C>` reading `C`, P3d) stays out of scope and is decided separately.

## Decisions needed

1. Choose option i, ii or iii.
2. Approve the spec clause for `base` and the fix or clause for `13-presentation.md:74-79`.
3. Confirm the keyword `base`, given the note at `kernel-and-library.md:555`.

## Not verified

- Whether the current checker already accepts `self.base`. The G5 probe reference (`C:lib.rs:5224-5230`) was not re-run for this document.
- Whether any `pub` refined type in the pinned stdlib would need `base` exposed under option iii. Not measured.
