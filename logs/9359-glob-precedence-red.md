# Glob precedence red run (PR 9359)

Mutation: removed the `if explicit.contains_key(&alias) { continue; }` guard in
`crates/orna-application-v1/src/lib.rs` wildcard binding loop (cut and restored).

Command (exit 1, test failed as expected):
  cargo test -p orna-application-v1 --lib explicit_import_takes_precedence_over_a_glob_in_either_order

Result: `left: Some("std.b.f")`, `right: Some("std.math.f")` at lib.rs:4110.
The glob overrides the explicit import without the guard, proving the test
guards the rule. Guard restored; `git status` shows no application-crate diff.
