#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
pattern='(^|[^[:alnum:]_])(QuotedIdentifier|TypeName|FunctionName|VariableName|NamespaceName|PropertyName|NumberLiteral|StringLiteral|SqlQueryBody|ClientFunction|ClientExpression|CapabilitySpecification|Word)([^[:alnum:]_]|$)|"(CREATE|SCHEMA|OBJECT|SELECT|INSERT|UPDATE|DELETE|CLIENT|SERVER|OPAQUE|CURRENCY|INGEST|STORE|VIEW|TRANSACTION|ENSURE|FACT|CONSTRAINTS?|UNIQUE|WHERE|CHECK)([^[:alnum:]_]|")'

for tree in editors packaging crates/orna-lsp/tests/fixtures; do
    if matches="$(grep -R -n -E -I "$pattern" "$root/$tree" 2>/dev/null)"; then
        if [[ -n "$matches" ]]; then
            printf 'legacy syntax found in %s:\n%s\n' "$tree" "$matches" >&2
            exit 1
        fi
    fi
done

retired_sources=(
    crates/orna-syntax/src/grammar.rs
    crates/orna-syntax/src/highlight.rs
    crates/orna-syntax/templates
    crates/orna-syntax/examples/generate_editor_artifacts.rs
    crates/orna-syntax/tests/editor_artifacts.rs
    crates/orna-lsp/tests/fixtures/tree-sitter-corpus
)
for path in "${retired_sources[@]}"; do
    if [[ -e "$root/$path" ]]; then
        printf 'retired hand-maintained editor source returned: %s\n' "$path" >&2
        exit 1
    fi
done

printf 'editor syntax purge guard passed\n'
