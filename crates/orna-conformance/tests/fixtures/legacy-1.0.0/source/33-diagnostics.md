# 33. Diagnostic reference {#diagnostics}

## Invalid forms and corrections

| Invalid form | Corresponding Orna form | Diagnostic |
|---|---|---|
| `fn f(...) -> T` | `fn f(...): T` | `ORNA091-E-RETURN-ARROW` |
| `var x = v;` | `let x = v;` | `ORNA091-E-VAR` |
| `match x { p => y }` | `case x { p: y }` | `ORNA091-E-MATCH` |
| `Result<T,E>`, `Ok`, `Err` | successful `T` plus automatic failure | `ORNA091-E-RESULT` |
| `operation?` | `operation` (automatic propagation) | `ORNA091-E-POSTFIX-QUESTION` |
| `operation` recovery via `match Result` | `operation |? handler` | `ORNA091-E-RESULT` |
| `pub currency GBP { ... }` | `pub type GBP { impl Currency { ... } }` | `ORNA091-E-CURRENCY` |
| `static symbol` currency identity | locale-aware `std.money.format` data | `ORNA091-E-CURRENCY-SYMBOL` |
| `impl P for T { ... }` | nested `impl P { ... }` inside `T` | `ORNA091-E-IMPL-FOR` |
| `<T: P>` | `<T impl P>` | `ORNA091-E-BOUND-COLON` |
| `static fn make(...): T` in a protocol | static property or ordinary function | `ORNA091-E-STATIC-FN` |
| `TryFrom<S>` | `From<S>` that may fail | `ORNA091-E-TRYFROM` |
| implicit `A` to `B` to `C` conversion search | explicit named conversion steps | `ORNA091-E-CONVERSION-CHAIN` |
| `opaque Name` | nominal `type Name { ... }` | `ORNA091-E-OPAQUE` |
| `field: T unique` | table `assert all_unique(...)` | `ORNA091-E-FIELD-CONSTRAINT` |
| `field: T check(expr)` | table `assert every(...)` | `ORNA091-E-FIELD-CONSTRAINT` |
| `type T = B where self ...` | refined brace assertion block | `ORNA-A091-001` |
| `assert self | predicate` | `assert predicate` in owner | `ORNA-A091-002` |
| `assert Table | predicate` | `assert predicate` in table | `ORNA-A091-002` |
| one-table module `assert` | move into table body | `ORNA-A091-003` |
| `ensure`, `fact`, `constraint`, `constraints`, `|!` | `assert` | `ORNA-A091-010` |
| `assert p else e` | ordinary recovery/control flow | `ORNA-A091-006` |



## Automated corrections

**ORNA-MIGRATE-003** Removal of postfix `?`, replacement of a function return arrow, and `var` to `let` are mechanical only when token-aware parsing proves the context.

**ORNA-MIGRATE-004** A tool MUST NOT rewrite anonymous-function `=>` while migrating case arms.


## Diagnostic classifications

The following diagnostic classifications apply to current source. Diagnostic codes are stable primary classifications; implementations may add structured notes and secondary spans.

| Code | Trigger | Primary remedy |
|---|---|---|
| `ORNA-A091-001` | refined type uses `where self` | use a brace-delimited `assert` block |
| `ORNA-A091-002` | table assertion begins with `self |` or its table name | remove the redundant owner pipeline |
| `ORNA-A091-003` | module assertion depends on only one table | move it into that table |
| `ORNA-A091-004` | declaration predicate is incompatible with its owner subject | supply a predicate for the reported owner type |
| `ORNA-A091-005` | assertion semicolon is missing | terminate the clause with `;` |
| `ORNA-A091-006` | assertion-specific `else` appears | use ordinary failure/recovery or control flow |
| `ORNA-A091-007` | declaration assertion has a forbidden effect or nondeterminism | remove the identified effect |
| `ORNA-A091-008` | executable or refined assertion is false | inspect owner, proposition and safe value |
| `ORNA-A091-009` | table or cross-table assertion is false | inspect the deterministic safe witness |
| `ORNA-A091-010` | unsupported assertion construct | use the sole `assert` form |
| `ORNA-A091-011` | assertion is empty | provide a proposition |
| `ORNA-A091-012` | module assertion has no table dependency | place the check in executable/test code |
| `ORNA091-E-RETURN-ARROW` | function return uses `->` | replace it with `:` |
| `ORNA091-E-VAR` | local declaration uses `var` | use `let`; assignment may replace the slot |
| `ORNA091-E-MATCH` | value branching uses `match`/arrow arms | use `case` and colon arms |
| `ORNA091-E-RESULT` | source uses core `Result`, `Ok`, or `Err` types | return the successful type; use automatic failure and `|?` |
| `ORNA091-E-POSTFIX-QUESTION` | expression uses postfix propagation `?` | remove it; failures propagate automatically |
| `ORNA091-E-CURRENCY` | special `currency` declaration | use a nominal type with nested `Currency` implementation |
| `ORNA091-E-CURRENCY-SYMBOL` | `Currency` declares a universal static symbol | move symbols/placement to locale formatter data |
| `ORNA091-E-IMPL-FOR` | top-level `impl P for T` | nest `impl P` inside `T` |
| `ORNA091-E-BOUND-COLON` | generic bound uses `<T: P>` | use `<T impl P>` |
| `ORNA091-E-STATIC-FN` | protocol declares a static function | use a static property or ordinary function |
| `ORNA091-E-TRYFROM` | source uses `TryFrom<S>` | use `From<S>`; it may fail |
| `ORNA091-E-CONVERSION-CHAIN` | context requires an unspoken multi-step conversion | name each conversion explicitly |
| `ORNA091-E-OPAQUE` | source uses an `opaque` type declaration | use the unified nominal/refined `type` form |
| `ORNA091-E-FIELD-CONSTRAINT` | field uses `unique` or `check(...)` | write an owner-local table assertion |

Diagnostics for secrets MUST redact sensitive values. Fixes MUST be token-aware: they may not rewrite anonymous-function `=>`, optional type suffixes `T?`, option coalescing `??`, or arbitrary ordinary pipelines merely because similar tokens occur nearby.

