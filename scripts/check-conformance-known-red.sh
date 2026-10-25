#!/usr/bin/env bash
# Gate for the orna-conformance-v1 test suite against scripts/conformance-known-red.txt.
#
# Each non-comment line of the list is:
#   <test name><TAB><bead id><TAB><reason><TAB><expected failure substring>.
# The bead id MUST match ^ornadb-[a-z0-9.-]+$ and appear in
# scripts/conformance-known-red-beads.txt. The expected substring is the first
# message line of the test's panic (captured by --collect); a listed test that fails
# with a different message fails the gate.
# For every test target (lib plus each tests/*.rs) the gate:
#   1. runs the target with --exact --skip for the listed names that belong to it;
#      any failure there is a NEW failure not in the list and fails the gate;
#   2. runs each listed name alone with --exact; a listed name that PASSES fails the
#      gate, and a listed name whose output lacks its expected substring fails the
#      gate. A listed name that matches no test in any target also fails the gate.
# The list may hold at most CONFORMANCE_KNOWN_RED_MAX entries (set in the workflow).
#
# usage: scripts/check-conformance-known-red.sh [--collect]
#   --collect  run every target without skips and print "name<TAB>expected substring"
#              for each failing test (used to regenerate the list; does not check it).
# Env: CARGO_TARGET_DIR, CARGO_BUILD_JOBS, TMPDIR and RUST_TEST_THREADS are honoured.
#      CONFORMANCE_KNOWN_RED_MAX is required for the check (not for --collect).
set -uo pipefail

cd "$(dirname "$0")/.." || exit 2
LIST=scripts/conformance-known-red.txt
BEADS=scripts/conformance-known-red-beads.txt
PKG=orna-conformance-v1
CARGO=(cargo test --locked -p "$PKG")
mode=check
[ "${1:-}" = "--collect" ] && mode=collect

build_log=$(mktemp)
if ! cargo test --locked -p "$PKG" --no-run >"$build_log" 2>&1; then
    cat "$build_log"
    rm -f "$build_log"
    echo "FAIL: $PKG test targets do not compile" >&2
    exit 1
fi
rm -f "$build_log"

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
        out=$(mktemp)
        # shellcheck disable=SC2046
        "${CARGO[@]}" $(target_args "$t") -- >"$out" 2>&1
        # Failing names, each with the first panic message line of its stdout block.
        awk '
            /^---- .* stdout ----$/ { name = $2; pending = 1; next }
            pending && /^thread .* panicked at / { pending = 0; want = 1; next }
            want { print name "\t" $0; want = 0 }
        ' "$out" >"$out.msgs"
        sed -n 's/^test \(.*\) \.\.\. FAILED$/\1/p' "$out" | while IFS= read -r n; do
            msg=$(awk -F'\t' -v n="$n" '$1 == n { print $2; exit }' "$out.msgs")
            printf '%s\t%s\n' "$n" "$msg"
        done
        rm -f "$out" "$out.msgs"
    done | sort -u
    exit 0
fi

if [ ! -f "$LIST" ]; then
    echo "FAIL: missing $LIST" >&2
    exit 1
fi
if [ -z "${CONFORMANCE_KNOWN_RED_MAX:-}" ]; then
    echo "FAIL: CONFORMANCE_KNOWN_RED_MAX must be set (the workflow sets it)" >&2
    exit 1
fi

# Known-red rows: name, bead, reason, expected substring. Validated strictly.
known=()
expect=()
while IFS= read -r line; do
    case "$line" in ''|'#'*) continue ;; esac
    IFS=$'\t' read -r name bead reason sub extra <<<"$line"
    if [ -z "${name:-}" ] || [ -z "${bead:-}" ] || [ -z "${reason:-}" ] || [ -z "${sub:-}" ] || [ -n "${extra:-}" ]; then
        echo "FAIL: malformed line in $LIST (need name<TAB>bead<TAB>reason<TAB>expected substring): $line" >&2
        exit 1
    fi
    if ! [[ "$bead" =~ ^ornadb-[a-z0-9.-]+$ ]]; then
        echo "FAIL: bead column is not an ornadb bead id ($bead): $name" >&2
        exit 1
    fi
    if ! grep -qxF -- "$bead" "$BEADS"; then
        echo "FAIL: bead $bead is not in $BEADS (adding a bead id needs a reviewed change to that file): $name" >&2
        exit 1
    fi
    known+=("$name")
    expect+=("$sub")
done <"$LIST"

errors=0
if [ "${#known[@]}" -gt "$CONFORMANCE_KNOWN_RED_MAX" ]; then
    echo "FAIL: $LIST has ${#known[@]} entries, above the cap CONFORMANCE_KNOWN_RED_MAX=$CONFORMANCE_KNOWN_RED_MAX; the list may only shrink (raise only with owner approval in the PR description)" >&2
    errors=$((errors + 1))
fi

declare -A owner=()
for t in "${targets[@]}"; do
    while IFS= read -r n; do
        owner["$n"]="$t"
    done < <(list_tests "$t")
done

for k in "${known[@]}"; do
    if [ -z "${owner[$k]:-}" ]; then
        echo "FAIL: listed test name matches no test in any target (renamed or removed? update $LIST): $k" >&2
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

# Step 2: every listed test must still fail when run alone, with its expected message.
for i in "${!known[@]}"; do
    k="${known[$i]}"
    sub="${expect[$i]}"
    t="${owner[$k]:-}"
    [ -z "$t" ] && continue
    out=$(mktemp)
    # shellcheck disable=SC2046
    if "${CARGO[@]}" $(target_args "$t") -- --exact "$k" >"$out" 2>&1; then
        echo "FAIL: listed test now PASSES (remove it from $LIST): $k" >&2
        errors=$((errors + 1))
    elif ! grep -F -q -- "$sub" "$out"; then
        echo "FAIL: listed test fails for an unexpected reason (expected message substring not found: $sub): $k" >&2
        errors=$((errors + 1))
    fi
    rm -f "$out"
done

if [ "$errors" -ne 0 ]; then
    echo "conformance known-red gate: $errors error(s)" >&2
    exit 1
fi
echo "conformance known-red gate: ok (${#known[@]} listed known-red tests, all still failing as expected)"
