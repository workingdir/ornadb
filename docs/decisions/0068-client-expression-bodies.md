# ADR 0068: CLIENT Expression Bodies and RUNTIME CONTRACT Clauses

**Status:** Accepted

**Implementation status:** The expression-body and `RUNTIME CONTRACT` slices are
implemented. The CLIENT-to-SERVER resource language, transport, and action
portions that were deferred here are now accepted and implemented by work ADRs
0077, 0078, and 0079; this ADR remains authoritative for the closed
same-domain expression and external-contract forms.

## Decision

CLIENT functions gain two closed surface extensions (spec roadmap milestone
7, and the tracked next step named in work ADR 0062:177-182):

1. **Expression bodies** - `CREATE CLIENT FUNCTION ... AS <expression> ;`
   replaces the Boolean-literal-only body with a closed expression language
   that the local `orna` client evaluates.
2. **External CLIENT functions with RUNTIME CONTRACT** -
   `CREATE EXTERNAL CLIENT FUNCTION ... RUNTIME CONTRACT '<name>@<version>' ;`
   declares a runtime-provided function with no body.

Both forms are already present in the spec EBNF
(`spec/spec/orna.ebnf:110-124`), the DDL reference
(`spec/docs/22-ddl-reference.md:88-125`), and the tree-sitter grammar
(`editors/tree-sitter-orna/grammar.js:225-260`); the hand-written parser,
compiler, and client evaluator implement none of them.

## Expression surface

A CLIENT expression body is one expression with these closed forms:

```text
expression     := call_expression | string_literal | integer_literal
                | boolean_literal | parameter_read | field_path
                | concatenation
call_expression := qualified_name "(" [ argument_list ] ")"
argument_list  := argument { "," argument }
argument       := expression                      (positional)
                | identifier "=>" expression      (named)
field_path     := parameter_identifier "." identifier { "." identifier }
concatenation  := expression "||" expression
```

The closed rules:

- A call resolves to a CLIENT function in the active application catalogue
  (same-domain calls only). Cross-domain CLIENT-to-SERVER async calls, resources,
  and `AWAIT` were deferred by this slice; their accepted language and
  transport contracts are defined by work ADRs 0077 and 0078.
- Named arguments bind by parameter identity; positional arguments bind in
  declaration order; the compiler rejects unknown, duplicate, missing, or
  trailing arguments with the declaration span.
- Literals are the language's canonical scalar forms (string, integer,
  boolean); a parameter read names a declared parameter; a field path
  starts at a declared parameter and reads fields of a referenced object.
- Concatenation is left-associative and requires both sides to be text.
- Every call adds a durable reference from the CLIENT function to the
  called function (kind `FunctionCall`), so semantic renames and diffs
  track CLIENT call graphs exactly like SERVER ones.

## RUNTIME CONTRACT surface

```sql
CREATE EXTERNAL CLIENT FUNCTION std.ui.window (
    title   TEXT,
    content std.ui.UI
)
RETURNS std.ui.UI
RUNTIME CONTRACT 'std.ui.window@1';
```

Closed rules:

- `EXTERNAL` appears between `CREATE` and `CLIENT`; the clause
  `RUNTIME CONTRACT '<identity>'` appears after `RETURNS` and before an
  optional capability clause; the statement ends with `;` and has no body.
- The contract identity is one string literal matching
  `<name>@<version>` (a qualified name, `@`, and a positive integer
  version). Any other shape fails closed at compile time.
- The compiler retains the contract identity on the function declaration
  and records it in the prepared CLIENT artifact. An external function is
  a real catalogue function with a real revision; its body is the
  declared contract, not source.
- Evaluation without a runtime that offers the contract fails closed: the
  local client evaluates CLIENT bodies it understands and rejects an
  external body whose contract no installed runtime offers, with a closed
  rule naming the contract identity. The tty runtime offers no external
  contract in this slice, so every external CLIENT invocation fails closed
  with that rule until a contract-offering runtime lands.

## Artifact

`orna.client-plan` gains version 3: one checked expression tree.

```text
magic[8] = ORNACP\0\0
version: u32 big-endian = 3
operation: u8 = 3
tree: canonical recursive encoding
```

The tree nodes are: `Call { function: FunctionId, arguments:
[(ParameterId, Expr)] }`, `StringLiteral { bytes }`,
`IntegerLiteral { i64 }`, `BooleanLiteral { bool }`,
`ParameterRead { ParameterId }`, `FieldPath { ParameterId, fields:
[FieldId] }`, `Concat { left, right }`. Node encoding is
length-prefixed and depth-bounded by the existing artifact size and node
limits; decode validates every identity and the closed shape.

The RUNTIME CONTRACT form uses the same version-3 container with a
distinct `ExternalContract` node carrying the contract identity string.

## Compiler

`check_client_functions` (orna-compiler/src/resolver.rs:4015) gains:

- expression-body acceptance: every declared capability must be exercised
  by a call in the body (over-declaration fails closed exactly as the
  current Boolean body rule);
- call resolution: each callee must resolve to a CLIENT function in the
  active catalogue with the exact argument shape;
- the closed expression rules above;
- retention of the contract identity for external functions.

`prepare.rs` emits the version-3 artifact and the new `FunctionCall`
references.

## Client evaluator

`orna-client` (evaluate_client_function_with_grants, lib.rs:365) gains a
version-3 evaluation path:

- literal nodes produce the canonical scalar value;
- parameter reads bind from the supplied typed arguments (the sealed and
  raw dispatch paths now pass the bound `FunctionArgument` list);
- field paths read fields of the resolved object value;
- call nodes recursively evaluate the called CLIENT function's current
  revision plan with the bound arguments, under the same capability gate;
- concatenation evaluates both sides and requires text;
- external-contract nodes fail closed with the contract rule when no
  installed runtime offers the contract.

Recursion is bounded by the existing artifact node limits and a closed
call-depth cap.

The raw dispatch path also passes trusted `FunctionArgument` values to the
evaluator. A parameterised expression function can therefore run through raw
dispatch. A non-empty argument list for a parameter-free CLIENT function
remains a closed `TARGET_UNAVAILABLE` target-shape failure.

## Required implementation order

1. `docs(client): define CLIENT expression bodies and RUNTIME CONTRACT` -
   this ADR and the work-ADR index only.
2. `feat(syntax): parse CLIENT expression bodies and RUNTIME CONTRACT` -
   the `ClientFunctionBody::Expression`/`ExternalContract` forms, the
   `AS` clause, the `EXTERNAL`/`RUNTIME CONTRACT` clause, and parse tests.
3. `feat(artifact): client-plan version 3 expression trees` - the checked
   expression encoding, decode validation, and tests.
4. `feat(compiler): check CLIENT expression bodies` - call resolution,
   argument binding, closed expression rules, contract-identity
   validation, and rejection tests.
5. `feat(compiler): emit version-3 CLIENT artifacts and call references` -
   prepare.rs lowering and the `FunctionCall` reference records.
6. `feat(client): evaluate version-3 CLIENT plans` - the closed evaluator
   for literals/reads/paths/calls/concat and the external-contract
   fail-closed rule, with unit tests.
7. `test(server): prove CLIENT expression evaluation end to end` - a live
   proof installing one CLIENT function with an expression body (a call
   returning a literal-derived value) and one external contract function,
   then invoking both through the installed host: the expression function
   completes with its value, the external function fails closed with the
   contract rule.

Each commit changes one to three files, uses a Conventional Commit, and
keeps the workspace buildable.

## Deferred surface

LOCAL/SESSION state declarations, function-instance identity, the sandbox body
forms, trace hooks, and the graphical runtime contract set remain outside this
ADR. The CLIENT-to-SERVER resource language and transport, including `AWAIT` and
streams, are implemented by work ADRs 0077 and 0078; action values are
implemented by ADR 0079. SERVER functions gain no expression bodies in this
slice. The expression vocabulary is closed to the forms above until a later
ADR extends it.

## Precedence

This decision extends work ADR 0060 (capability gate) and implements the
expression path named as the next step in work ADR 0062:177-182. Work ADRs
0077-0079 now govern the accepted resource, transport, and action successors
to the deferred portions recorded above. Spec ADRs and docs remain
authoritative outside this accepted implementation scope; the EBNF and DDL
reference define the surface this decision makes executable.