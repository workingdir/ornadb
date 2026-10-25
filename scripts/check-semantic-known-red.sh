#!/usr/bin/env bash
# Gate for the orna-semantic-v1 test suite against scripts/semantic-known-red.txt.
#
# Each non-comment line of the list is: <test name><TAB><bead id><TAB><reason>.
# For every test target (lib plus each tests/*.rs) the gate:
#   1. runs the target with --exact --skip for the listed names that belong to it;
#      any failure there is a NEW failure not in the list and fails the gate;
#   2. runs each listed name alone with --exact; a listed name that PASSES fails the
#      gate, so the list shrinks as the bead fixes land. A listed name that matches no
#      test in any target also fails the gate.
# Together these make the failing set of the crate equal the list exactly.
#
# usage: scripts/check-semantic-known-red.sh [--collect]
#   --collect  run every target without skips and print the failing test names
#              (used to regenerate the list; does not check it).
# Env: CARGO_TARGET_DIR, CARGO_BUILD_JOBS and RUST_TEST_THREADS are honoured.
set -uo pipefail

cd "$(dirname "$0")/.." || exit 2
LIST=scripts/semantic-known-red.txt
PKG=orna-semantic-v1
CARGO=(cargo test --locked -p "$PKG")
mode=check
[ "${1:-}" = "--collect" ] && mode=collect

if ! cargo test --locked -p "$PKG" --no-run >/tmp/semantic-known-red-build.log 2>&1; then
    cat /tmp/semantic-known-red-build.log
    echo "FAIL: $PKG test targets do not compile" >&2
    exit 1
fi

targets=(lib)
for f in crates/$PKG/tests/*.rs; do
    targets+=("$(basename "$f" .rs)")
done

target_args() {
    if [ "$1" = lib ]; then echo "--lib"; else echo "--test $1"; fi
}

# Names of every test in a target, one per line.
list_tests() {
    # shellcheck disable=SC2046
    "${CARGO[@]}" $(target_args "$1") -- --list 2>/dev/null | sed -n 's/: test$//p'
}

if [ "$mode" = collect ]; then
    for t in "${targets[@]}"; do
        # shellcheck disable=SC2046
        "${CARGO[@]}" $(target_args "$t") -- 2>&1 | sed -n 's/^test \(.*\) \.\.\. FAILED$/\1/p'
    done | sort -u
    exit 0
fi

if [ ! -f "$LIST" ]; then
    echo "FAIL: missing $LIST" >&2
    exit 1
fi

# Known-red names (column 1), validated: exactly three tab-separated fields.
known=()
while IFS= read -r line; do
    case "$line" in ''|'#'*) continue ;; esac
    IFS=$'\t' read -r name bead reason extra <<<"$line"
    if [ -z "${name:-}" ] || [ -z "${bead:-}" ] || [ -z "${reason:-}" ] || [ -n "${extra:-}" ]; then
        echo "FAIL: malformed line in $LIST (need name<TAB>bead<TAB>reason): $line" >&2
        exit 1
    fi
    known+=("$name")
done <"$LIST"

errors=0
declare -A owner=()
for t in "${targets[@]}"; do
    while IFS= read -r n; do
        owner["$n"]="$t"
    done < <(list_tests "$t")
done

for k in "${known[@]}"; do
    if [ -z "${owner[$k]:-}" ]; then
        echo "FAIL: listed test matches no test in any target: $k" >&2
        errors=$((errors + 1))
    fi
done

# Step 1: no unlisted failures. Skip only the listed names that belong to this target.
for t in "${targets[@]}"; do
    skips=()
    for k in "${known[@]}"; do
        [ "${owner[$k]:-}" = "$t" ] && skips+=(--skip "$k")
    done
    out=$(mktemp)
    # shellcheck disable=SC2046
    "${CARGO[@]}" $(target_args "$t") -- --exact "${skips[@]}" >"$out" 2>&1
    rc=$?
    if [ "$rc" -ne 0 ]; then
        echo "target $t: exit $rc with listed tests skipped"
        grep -E '^test .* \.\.\. FAILED$' "$out" | sed 's/^/  UNLISTED FAILURE /'
        if ! grep -q -E '^test .* \.\.\. FAILED$' "$out"; then
            tail -20 "$out" | sed 's/^/  /'
        fi
        errors=$((errors + 1))
    else
        echo "target $t: no unlisted failures ($(grep -E '^test result' "$out"))"
    fi
    rm -f "$out"
done

# Step 2: every listed test must still fail when run alone.
for k in "${known[@]}"; do
    t="${owner[$k]:-}"
    [ -z "$t" ] && continue
    # shellcheck disable=SC2046
    if "${CARGO[@]}" $(target_args "$t") -- --exact "$k" >/dev/null 2>&1; then
        echo "FAIL: listed test now PASSES (remove it from $LIST): $k" >&2
        errors=$((errors + 1))
    fi
done

if [ "$errors" -ne 0 ]; then
    echo "semantic known-red gate: $errors error(s)" >&2
    exit 1
fi
echo "semantic known-red gate: ok (${#known[@]} listed known-red tests, all still failing)"
