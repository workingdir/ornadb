#!/usr/bin/env bash
# Gate A item A2: the shipped orna-storage crate must carry no retired row
# writer (ORNA-UPGRADE-003, one writer; source/40-upgrade-contract.md).
#
# The retired compact writer boundary lives behind the `compact-writer-fixtures`
# feature because two test files need it to build a compact base state. That
# keeps the code in the tree, so the crate has to be held to the claim that the
# default build contains none of it. This script builds both ways and compares
# the symbols in the library:
#
#   1. with the feature, every guarded entry point must appear in the rlib, so
#      the check proves it can see a writer when one is compiled in;
#   2. with default features, none of them may appear.
#
# A check that only looked at the default build could pass because the search
# was wrong. Step 1 is what makes step 2 mean something.
#
# usage: check_retired_row_writers.sh
# Env: CARGO_TARGET_DIR is honoured; a private directory keeps the two builds
#      from disturbing another job's artefacts.

set -uo pipefail

cd "$(dirname "$0")/.." || exit 2

TARGET="${CARGO_TARGET_DIR:-target}"
RLIB="$TARGET/debug/liborna_storage.rlib"
SYMBOLS=(
    apply_writer_input
    apply_writer_batch
    prepare_writer_batch
    consume_writer_input
    fold_writer_input
    lower_publication_freeze
    apply_migration_plan_to_compact
    apply_evolution_plan
)

# The authority is the defined symbol table, not a plain string scan: crate
# metadata keeps unrelated identifiers as text, which a string scan would count
# as a writer that is not there. Rust mangles the identifiers into the symbol
# names, so the scan greps the mangled form without demangling.
if ! command -v nm >/dev/null 2>&1; then
    echo "FAIL: nm is required to read the symbol table" >&2
    exit 2
fi

scan() {
    local symbol count total=0
    for symbol in "${SYMBOLS[@]}"; do
        count=$(nm --defined-only "$RLIB" 2>/dev/null | grep -c -- "$symbol")
        printf '  %-34s %s\n' "$symbol" "$count"
        total=$((total + count))
    done
    echo "$total"
}


# A build that fails silently leaves the caller with "missing rlib; build
# orna-storage first", which names the wrong cause. Nightly run 41614410003
# failed this check as `command exited 2` with nothing else in the log, because
# both attempts wrote to /dev/null.
build() {
    local log
    log=$(mktemp)
    if cargo build --locked --offline "$@" >"$log" 2>&1 || cargo build --locked "$@" >>"$log" 2>&1; then
        rm -f "$log"
        return 0
    fi
    echo "FAIL: cargo build $* did not succeed; last 40 lines of its output:" >&2
    tail -n 40 "$log" >&2
    rm -f "$log"
    return 1
}

echo "check retired-row-writers: with compact-writer-fixtures"
# The library must be built before it can be scanned. The old order checked for
# the rlib first and exited 2 on a clean runner, so the check never ran at all.
build -p orna-storage --features compact-writer-fixtures || exit 2
if [ ! -f "$RLIB" ]; then
    echo "FAIL: the guarded build reported success but $RLIB is absent" >&2
    exit 2
fi
if ! present=$(scan | tail -1) || [ "$present" -eq 0 ]; then
    echo "FAIL: the guarded writer symbols are absent even with compact-writer-fixtures, so this check cannot see a compiled writer (found $present)" >&2
    exit 1
fi
echo "  total=$present (expected above 0)"

echo "check retired-row-writers: default features"
build -p orna-storage || exit 2
if ! absent=$(scan | tail -1); then
    echo "FAIL: could not scan $RLIB" >&2
    exit 1
fi
echo "  total=$absent (expected 0)"
if [ "$absent" -ne 0 ]; then
    echo "FAIL: the shipped crate carries a retired row writer; keep it behind compact-writer-fixtures or delete it" >&2
    exit 1
fi

echo "check retired-row-writers: passed; the shipped crate carries no retired writer and the guarded build carries $present"
