# 3. Source, modules and namespaces {#source-modules}

## File extension

Module units and loose row units both use `.orna` because both contain Orna syntax.

**ORNA-SOURCE-001** The parser MUST expose distinct entrypoints for module units and row units.

**ORNA-SOURCE-002** A module unit MUST contain declarations only at top level.

**ORNA-SOURCE-003** A row unit MUST contain exactly one record expression and MUST NOT contain declarations.

## Namespace mapping

Filesystem structure defines module namespaces.

```text
sensors/greenhouse/main.orna       -> sensors.greenhouse
sensors/greenhouse/input.orna -> sensors.greenhouse.input
```

**ORNA-NS-001** A directory's `main.orna` MUST define that directory's namespace.

**ORNA-NS-002** A non-`main.orna` module filename MUST add its stem as the final namespace component.

**ORNA-NS-003** The repository root `main.orna` MUST define the root namespace.

**ORNA-NS-004** A repository MUST NOT contain both `x.orna` and `x/main.orna` where both would define the same namespace.

## Reserved namespaces

**ORNA-NS-005** `sys` and `std` are the only reserved top-level namespaces.

**ORNA-NS-006** `sys` MUST be built into every conforming implementation and MUST NOT be replaced or shadowed.

**ORNA-NS-007** `std` MUST remain optional and replaceable, although its top-level name is reserved when present.

**ORNA-NS-008** Source/module/table path components MUST be NFC and siblings MUST be unique under Unicode 16.0.0 toNFKC_Casefold. The host-independent check applies during load, rename and merge; a filesystem that distinguishes colliding names does not make them portable.

## Imports

Valid forms:

```orna
use directory;
use sensors.greenhouse as climate;
use sensors.greenhouse.*;
use sensors.greenhouse.{Reading, recent};
use std.prelude as _;
```

**ORNA-IMPORT-001** `use a.b;` MUST make namespace `a.b` available under its final component unless it conflicts.

**ORNA-IMPORT-002** `use a.b as x;` MUST make the imported namespace available as `x`.

**ORNA-IMPORT-003** `use a.b.*;` MUST import all public names from `a.b` into the current lexical scope.

**ORNA-IMPORT-004** `use a.b.{X, y};` MUST import only the named public definitions.

**ORNA-IMPORT-005** `use x as _;` MUST import the prelude set explicitly exported by that pinned module, without binding a namespace alias. An absent prelude declaration imports no names; it does not mean wildcard import.

**ORNA-IMPORT-006** Importing a module MUST NOT execute arbitrary code, open a network connection or start a stream.

**ORNA-IMPORT-007** Wildcard imports are valid in committed modules and the REPL under the same rules. Local declarations and explicit imports take precedence; two wildcard imports that expose the same otherwise-unresolved name produce an ambiguity diagnostic. Import order MUST NOT select a winner.

