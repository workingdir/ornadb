#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
command=${1:-build}
if [[ $# -gt 0 ]]; then
    shift
fi
case "${command}" in
    build|verify|install)
        exec python3 "${script_dir}/package.py" "${command}" "$@"
        ;;
    test)
        exec "${script_dir}/test.sh" "$@"
        ;;
    help|-h|--help)
        exec python3 "${script_dir}/package.py" --help
        ;;
    *)
        printf '%s\n' "[linux-package] error: unknown command: ${command}" >&2
        printf '%s\n' "usage: package.sh [build|verify|install|test] [options]" >&2
        exit 2
        ;;
esac
