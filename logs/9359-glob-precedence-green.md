# Glob precedence green run (PR 9359)

Head: 6cac64053 (guard present, no mutation).
Command (exit 0): cargo test -p orna-application-v1 --lib explicit_import_takes_precedence_over_a_glob_in_either_order
Result: test tests::explicit_import_takes_precedence_over_a_glob_in_either_order ... ok; 1 passed; 0 failed.
Pairs with logs/9359-glob-precedence-red.md (guard removed, test fails).
