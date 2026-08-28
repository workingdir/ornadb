#!/usr/bin/env bash
set -euo pipefail
export PATH="/usr/local/bin:/usr/bin:/bin:${PATH:-}"

repository_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
mkdir -p -- "${repository_root}/target"
scratch=$(mktemp -d "${repository_root}/target/orna-local-cli-demo.XXXXXX")
state_home="${scratch}/state"
runtime_home="${scratch}/runtime"
server_log="${scratch}/server.log"
server_pid=""
ready_file=""

server_is_running() {
    [[ -n "${server_pid}" ]] || return 1
    kill -0 "${server_pid}" 2>/dev/null || return 1
    local state
    state=$(ps -o stat= -p "${server_pid}" 2>/dev/null || true)
    [[ -n "${state}" && "${state}" != Z* ]]
}

stop_server() {
    if server_is_running; then
        kill -INT "${server_pid}" 2>/dev/null || true
        for ((attempt = 0; attempt < 50; attempt += 1)); do
            if ! server_is_running; then
                break
            fi
            sleep 0.1
        done
        if server_is_running; then
            kill -TERM "${server_pid}" 2>/dev/null || true
            for ((attempt = 0; attempt < 50; attempt += 1)); do
                if ! server_is_running; then
                    break
                fi
                sleep 0.1
            done
        fi
        if server_is_running; then
            kill -KILL "${server_pid}" 2>/dev/null || true
        fi
    fi
    if [[ -n "${server_pid}" ]]; then
        wait "${server_pid}" || true
    fi
}

cleanup() {
    local status=$?
    trap - EXIT INT TERM
    stop_server
    if [[ -n "${runtime_home}" ]] && pgrep -af -- "${runtime_home}/orna/default" >/dev/null 2>&1; then
        printf '%s\n' '[local-cli-demo] a server child process remains after shutdown' >&2
        status=1
    fi
    rm -rf -- "${scratch}"
    exit "${status}"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

printf '%s\n' '[local-cli-demo] building the Orna binary'
cargo build --quiet --locked --manifest-path "${repository_root}/Cargo.toml" -p orna-server

printf '%s\n' '[local-cli-demo] starting a temporary local server'
XDG_STATE_HOME="${state_home}" XDG_RUNTIME_DIR="${runtime_home}" \
    "${repository_root}/target/debug/orna" server run >"${server_log}" 2>&1 &
server_pid=$!
ready_file="${runtime_home}/orna/default/ready"
for ((attempt = 0; attempt < 600; attempt += 1)); do
    if [[ -f "${ready_file}" ]]; then
        break
    fi
    if ! server_is_running; then
        printf '%s\n' '[local-cli-demo] server exited before readiness' >&2
        cat "${server_log}" >&2
        exit 1
    fi
    sleep 0.1
done
if [[ ! -f "${ready_file}" ]]; then
    printf '%s\n' '[local-cli-demo] server did not become ready' >&2
    cat "${server_log}" >&2
    exit 1
fi

printf '%s\n' '[local-cli-demo] invoking std.invoke.echo with p_value=7'
XDG_STATE_HOME="${state_home}" XDG_RUNTIME_DIR="${runtime_home}" \
    "${repository_root}/target/debug/orna" invoke std.invoke.echo --arg p_value=7
