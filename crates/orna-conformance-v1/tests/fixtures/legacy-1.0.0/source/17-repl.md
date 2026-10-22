# 17. The interactive session {#repl}

## Session model

The REPL behaves like an ephemeral module. Imports, bindings and function declarations persist for the session.

```text
> use sensors.greenhouse.*;
> let recent = Reading | filter(r => r.time > now() - 1.hour);
> fn warm() = recent | filter(r => r.temperature > 25.C);
```

**ORNA-REPL-001** The REPL MUST use ordinary `use` syntax for namespace access and MUST NOT require a separate `:cd` namespace model.

## Last result and status

```text
$_
```

is the last successful REPL result, inspired by shell conventions.

```text
$?
```

is the last execution status/error value if supported.

**ORNA-REPL-002** `$_` and `$?` are REPL bindings and MUST NOT become implicit globals in module source.

## Syntax-aware editing

The REPL SHOULD provide:

- syntax highlighting while typing;
- matching-brace highlighting;
- automatic indentation;
- multiline input until syntax is complete;
- context/type-aware completion;
- signatures and documentation in completion;
- clickable source locations;
- persistent history;
- a pager for large output.

## Eager previews

Safe previews show both value and type:

```text
3 : Int
"alice-smith" : Str
£12.34 : Money<GBP>
1.5 hour : Float<hour>
```

For a relation expression, a dim ghost preview may show:

```text
╰─ Relation<sensors.greenhouse.Reading> · ≈42.1k rows
```

The `≈` and ghost placement communicate estimation/laziness without prose such as “not executed”.

**ORNA-REPL-003** Eager preview MUST NOT perform mutations, external I/O or other effects.

**ORNA-REPL-004** A relation preview MUST NOT require complete enumeration.

After submission, the relation may be presented as a bounded table window and fetched incrementally during navigation.

## Default presentation

```text
> directory.Contact
```

may render:

```text
directory.Contact : Relation<Contact> · 428 rows · CWD

id                name             emails
alice-smith       Alice Smith      2
bob-jones         Bob Jones        1
...
```

The type remains visible.

## Console commands

Console operations are prefixed with `:` so they are not confused with language functions:

```text
:help [name]
:doc expression
:type expression
:source expression
:open expression
:inspect expression
:members expression
:table expression
:plan expression
:trace expression
:watch expression
:unwatch id
:at CWD|HEAD|ref
:history [query]
:copy expression [--format orna|json|csv]
:display Type formatter|default
:timing on|off
:clear
:quit
```

**ORNA-REPL-005** `:at` changes the default snapshot context for the session; it MUST NOT mutate repository HEAD.

**ORNA-REPL-006** `:watch` MUST use the same dependency and delta machinery as page sessions.

