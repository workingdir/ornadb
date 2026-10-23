# ADR 0002: Public Language Contract

**Status:** Accepted

## Decision

Orna relational queries use SQL:2023 Foundation Core plus explicitly named,
documented supported features. Orna extensions are explicit and do not change
the meaning of otherwise valid standard SQL. PostgreSQL syntax and behaviour
are not part of the normal Orna language contract.

The only type aliases are `BOOL` for `BOOLEAN`, `INT` for `INTEGER`, `TEXT`
for `CHARACTER LARGE OBJECT`, and `BYTES` for `BINARY LARGE OBJECT`. They
resolve to the same semantic `TypeId` as their canonical types while the
lossless source form retains the written alias.

Orna owns its lexer, source spans, lossless Rowan concrete syntax tree, typed
AST, resolution, and Orna-owned IR. The public catalogue and protocol expose
none of a parser implementation's types. `sqlparser-rs` may only be a
replaceable internal aid for relational expressions selected by Orna's
dialect and semantic allowlist.

Query-producing SERVER functions return `ROWS (...)`. `ROWS` denotes zero or
more shaped records. It preserves duplicates unless `DISTINCT` is requested,
and it has no order unless `ORDER BY` specifies one. `TABLE` and `SET OF` are
not return declarations.

Each durable object has an immutable, server-generated, opaque `ObjectId`.
It is an Orna scalar, not a public UUID or integer. `REF(alias)` is the
language form for an object's identity; it lowers to typed identity and an
internal PostgreSQL foreign key. Object declarations cannot declare
`PRIMARY KEY`; the compiler rejects it with an explanatory diagnostic.
`object_id` remains a legal user field name. Object fields are nullable by
default, and `NOT NULL`
makes a field mandatory. The semantic model represents nullable fields as
option values. Declared fields have no separate durable missing state.

CLIENT UI functions use `RETURNS UI` and an explicit `RETURN`:

```sql
CREATE CLIENT FUNCTION example()
RETURNS UI
RETURN std.ui.text('Example');
```

`UI` resolves through the standard prelude to the standard-library `TypeId`.
It is not a parser special case. The `AS expression` form is not supported for
CLIENT UI functions. The long form remains `IS BEGIN RETURN ...; END;`.

## Deferred syntax

The replacement surface syntax for angle-bracket generic types, query-body
spelling not settled here, additional value-type syntax such as persistence
modifiers, and further UI declaration syntax remain deferred. This record does
not select syntax for them.

## Precedence

This accepted amendment supersedes the conflicting or current-proposal parts
of these sources:

* `spec/spec/orna.ebnf` allows `TABLE`, `AS expression`, and qualified
  `std.ui.UI` in the current grammar proposal.
* `spec/docs/22-ddl-reference.md` and `spec/docs/23-function-language.md`
  use `RETURNS SET OF`, `RETURNS std.ui.UI`, and `AS` expression forms.
* `spec/docs/03-quick-tour.md`, `spec/docs/44-bob-first-week.md`, and
  `spec/docs/46-syntax-to-runtime-trace.md` use `TABLE`, `SET OF`, or the
  replaced CLIENT UI forms.
* `spec/adrs/0003-ui-is-a-built-in-value-type.md` and
  `spec/adrs/0012-std-ui-value-type.md` require the former public spelling.
* `spec/docs/12-object-relational-model.md` leaves nullable syntax open and
  describes physical reference storage only as an implementation direction.
* `spec/docs/41-open-questions.md` leaves language and type syntax open.

For this subject, this record has precedence over those sources and their
derived examples.
