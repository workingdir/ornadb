#!/bin/bash
set -euo pipefail
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

repository_root=$(cd "$(dirname "$0")/../../.." && pwd -P)
changelog="${repository_root}/packaging/debian/changelog"
source="$(dpkg-parsechangelog -l"${changelog}" -S Source)"
version="$(dpkg-parsechangelog -l"${changelog}" -S Version)"
architecture="$(dpkg --print-architecture)"
[[ "${architecture}" == 'amd64' ]] || {
    printf '%s\n' '[clean-machine] error: package proof requires amd64' >&2
    exit 1
}
package=${1:-"${repository_root}/target/debian-package/${source}_${version}_${architecture}.deb"}

[[ "${package}" == /* && -f "${package}" && ! -L "${package}" ]] || {
    printf '%s\n' '[clean-machine] error: package must be one absolute regular file' >&2
    exit 1
}

scratch=$(mktemp -d "${repository_root}/target/debian-clean-machine.XXXXXX")
image="orna-debian-clean-machine:$PPID-$$"
cleanup() {
    docker image rm --force "${image}" >/dev/null 2>&1 || true
    find "${scratch}" -mindepth 1 -delete
    rmdir "${scratch}"
}
trap cleanup EXIT

docker build --platform linux/amd64 --provenance=false --tag "${image}" \
    --file "${repository_root}/crates/orna-system-tests/assets/debian/Containerfile" "${scratch}"

docker run --rm --interactive --network=none --platform linux/amd64 \
    --volume "${package}:/proof/orna.deb:ro" \
    "${image}" /bin/bash -euo pipefail -s <<'TEST'
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
package=/proof/orna.deb
ready=/run/orna/default/ready
public_socket=/run/orna/default/orna.sock
instance=/var/lib/orna/instances/default
manifest="${instance}/instance.toml"

fail() {
    printf '%s\n' "[clean-machine] error: $*" >&2
    exit 1
}

run_as_orna() {
    /usr/bin/env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin \
        /usr/bin/setpriv --reuid="${orna_uid}" --regid="${orna_gid}" --clear-groups -- "$@"
}

wait_ready() {
    local process=$1
    for _ in $(seq 1 1200); do
        kill -0 "${process}" 2>/dev/null || fail 'server exited before readiness'
        [[ -f "${ready}" ]] && return
        sleep 0.05
    done
    fail 'server did not publish readiness'
}

stop_server() {
    local process=$1
    kill -INT "${process}"
    wait "${process}" || fail 'server did not stop cleanly'
    [[ ! -e "${ready}" ]] || fail 'server retained readiness after stop'
    [[ ! -e "${public_socket}" ]] || fail 'server retained public socket after stop'
}

require_one_executable_tree() {
    local server_pid postmaster_pid expected_identity
    server_pid=$(sed -n 's/^server_pid = //p' "${ready}")
    postmaster_pid=$(sed -n 's/^postmaster_pid = //p' "${ready}")
    [[ "${server_pid}" =~ ^[1-9][0-9]*$ && "${postmaster_pid}" =~ ^[1-9][0-9]*$ ]] ||
        fail 'ready process identities are malformed'
    expected_identity=$(stat -Lc '%d:%i' /usr/bin/orna)
    run_as_orna /usr/bin/env SERVER_PID="${server_pid}" POSTMASTER_PID="${postmaster_pid}" \
        EXPECTED_IDENTITY="${expected_identity}" python3 - <<'PY'
import os
from pathlib import Path

server = int(os.environ['SERVER_PID'])
postmaster = int(os.environ['POSTMASTER_PID'])
expected = os.environ['EXPECTED_IDENTITY']
pending = [server]
seen = set()
while pending:
    pid = pending.pop()
    if pid in seen:
        continue
    seen.add(pid)
    children = Path(f'/proc/{pid}/task/{pid}/children')
    if children.exists():
        pending.extend(int(value) for value in children.read_text().split())
if postmaster not in seen or len(seen) < 3:
    raise SystemExit('linked PostgreSQL descendant closure is incomplete')
for pid in seen:
    target = Path(f'/proc/{pid}/exe').stat()
    identity = f'{target.st_dev}:{target.st_ino}'
    if identity != expected:
        raise SystemExit(f'process {pid} has a second executable identity')
PY
}

dpkg --install "${package}"
[[ "$(dpkg-query -W -f='${db:Status-Status}' orna)" == installed ]] ||
    fail 'package is not installed'
[[ "$(stat -c '%U:%G %a' /usr/bin/orna)" == 'root:root 755' ]] ||
    fail 'installed executable metadata changed'
mapfile -t product_executables < <(
    find /usr -xdev -type f -perm /111 -path '*orna*' -print | LC_ALL=C sort
)
[[ "${product_executables[*]}" == /usr/bin/orna ]] ||
    fail 'installed product executable inventory is not exact'
[[ -z "$(find /usr -xdev -type f \
    \( -name postgres -o -name initdb -o -name psql -o -name pg_upgrade \
    -o -name '*.so' -o -name '*.so.*' -o -name '*.a' -o -name '*.o' \) \
    -path '*orna*' -print -quit)" ]] || fail 'PostgreSQL runtime artefact is installed'
mkdir -m 0700 -p /work
readelf --dynamic /usr/bin/orna >/work/orna.dynamic
! grep -Eqi 'NEEDED.*(libpq|libz|postgres)' /work/orna.dynamic ||
    fail 'installed Orna has a dynamic PostgreSQL dependency'

set +e
/usr/bin/env -i /usr/bin/orna server upgrade >/work/root-upgrade.stdout \
    2>/work/root-upgrade.stderr
root_upgrade_status=$?
set -e
[[ "${root_upgrade_status}" -eq 1 && ! -s /work/root-upgrade.stdout ]] ||
    fail 'root upgrade request did not fail closed'
[[ "$(cat /work/root-upgrade.stderr)" == \
    'orna: server upgrade must run as the orna service account' ]] ||
    fail 'root upgrade diagnostic changed'

orna_uid=$(id -u orna)
orna_gid=$(id -g orna)
[[ "${orna_uid}" -ne 0 && "${orna_gid}" -ne 0 ]] || fail 'service account is root'
distribution=/usr/share/orna/distribution-manifest.toml
cp --preserve=all "${distribution}" /work/distribution-manifest.toml
sed -i 's/^executable_sha256 = ".*"$/executable_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"/' \
    "${distribution}"
set +e
run_as_orna /usr/bin/orna server run >/work/distribution.stdout \
    2>/work/distribution.stderr
distribution_status=$?
set -e
[[ "${distribution_status}" -eq 1 && ! -s /work/distribution.stdout ]] ||
    fail 'changed distribution manifest did not fail closed'
[[ "$(cat /work/distribution.stderr)" == 'Orna distribution manifest is invalid' ]] ||
    fail 'distribution-manifest diagnostic changed'
[[ ! -e /var/lib/orna/instances/default ]] ||
    fail 'invalid distribution manifest reached instance creation'
install -o root -g root -m 0644 /work/distribution-manifest.toml "${distribution}"
chmod 0666 "${distribution}"
set +e
run_as_orna /usr/bin/orna server run >/work/distribution-mode.stdout \
    2>/work/distribution-mode.stderr
distribution_mode_status=$?
set -e
[[ "${distribution_mode_status}" -eq 1 && ! -s /work/distribution-mode.stdout ]] ||
    fail 'writable distribution manifest did not fail closed'
[[ "$(cat /work/distribution-mode.stderr)" == 'Orna distribution manifest is invalid' ]] ||
    fail 'distribution-manifest mode diagnostic changed'
[[ ! -e /var/lib/orna/instances/default ]] ||
    fail 'writable distribution manifest reached instance creation'
install -o root -g root -m 0644 /work/distribution-manifest.toml "${distribution}"
set +e
run_as_orna /usr/bin/orna server upgrade >/work/absent-upgrade.stdout \
    2>/work/absent-upgrade.stderr
absent_upgrade_status=$?
set -e
[[ "${absent_upgrade_status}" -eq 1 && ! -s /work/absent-upgrade.stdout ]] ||
    fail 'absent-instance upgrade did not fail closed'
[[ "$(cat /work/absent-upgrade.stderr)" == \
    'orna: the default Orna instance is not installed' ]] ||
    fail 'absent-instance diagnostic changed'

# Source check is a pure offline compiler check. It runs as the orna service
# account before any server process starts. No instance, ready file, or public
# socket exists yet, and the checks must not create one.
chmod 0711 /work
printf '%s' 'CREATE SCHEMA app; CREATE TYPE app.task AS OBJECT (done BOOLEAN NOT NULL);' \
    >/work/valid.orna
printf '%s' 'CREATE SCHEMA ;' >/work/invalid.orna
set +e
run_as_orna /usr/bin/orna source check /work/valid.orna \
    >/work/source-valid.stdout 2>/work/source-valid.stderr
source_valid_status=$?
set -e
[[ "${source_valid_status}" -eq 0 && ! -s /work/source-valid.stdout \
    && ! -s /work/source-valid.stderr ]] ||
    fail 'valid source check did not pass cleanly'
set +e
run_as_orna /usr/bin/orna source check /work/invalid.orna \
    >/work/source-invalid.stdout 2>/work/source-invalid.stderr
source_invalid_status=$?
set -e
[[ "${source_invalid_status}" -eq 1 && ! -s /work/source-invalid.stdout ]] ||
    fail 'invalid source check did not fail closed'
[[ "$(cat /work/source-invalid.stderr)" == \
    '/work/invalid.orna:14..15: ORNA0001: expected a schema name after CREATE SCHEMA' ]] ||
    fail 'source-check diagnostic changed'
[[ ! -e /var/lib/orna/instances/default ]] ||
    fail 'source check reached instance creation'
[[ ! -e /run/orna/default/ready && ! -e /run/orna/default/orna.sock ]] ||
    fail 'source check created a runtime artefact'
# Restore the private work-directory mode for the rest of the lifecycle.
chmod 0700 /work

install -d -o orna -g orna -m 0711 /run/orna/default
/usr/bin/env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin \
    /usr/bin/setpriv --reuid="${orna_uid}" --regid="${orna_gid}" --clear-groups -- \
    /usr/bin/orna server run >/work/server.stdout 2>/work/server.stderr &
server_process=$!
wait_ready "${server_process}"
require_one_executable_tree
[[ "$(stat -c '%U:%G %a %F' /run/orna/default)" == 'orna:orna 711 directory' ]] ||
    fail 'public runtime-root metadata changed'
[[ "$(stat -c '%U:%G %a %F %h' "${public_socket}")" == \
    'orna:orna 666 socket 1' ]] || fail 'public raw socket metadata changed'
[[ "$(stat -c '%U:%G %a %F' /run/orna/default/postgres)" == \
    'orna:orna 700 directory' ]] || fail 'private PostgreSQL socket metadata changed'
[[ "$(stat -c '%U:%G %a %F' "${ready}")" == 'orna:orna 600 regular file' ]] ||
    fail 'private readiness metadata changed'

health_function_id=function:00000000000000000000000004
run_as_orna /usr/bin/orna raw-call sys.catalog.health \
    >/work/catalogue-health-name.stdout 2>/work/catalogue-health-name.stderr ||
    fail 'catalogue health name call failed'
run_as_orna /usr/bin/orna raw-call "${health_function_id}" \
    >/work/catalogue-health-id.stdout 2>/work/catalogue-health-id.stderr ||
    fail 'catalogue health identity call failed'
[[ ! -s /work/catalogue-health-name.stderr && ! -s /work/catalogue-health-id.stderr ]] ||
    fail 'catalogue health call produced a diagnostic'
cmp /work/catalogue-health-name.stdout /work/catalogue-health-id.stdout >/dev/null ||
    fail 'catalogue health name and identity outputs differ'
python3 - <<'PY'
from pathlib import Path

expected = b'ORV1' + bytes([2]) + bytes(15) + bytes([1]) + bytes([0, 0, 0, 1, 1])
actual = Path('/work/catalogue-health-name.stdout').read_bytes()
if actual != expected:
    raise SystemExit('catalogue health output is not the exact Boolean TRUE envelope')
PY
require_one_executable_tree

set +e
run_as_orna /usr/bin/orna server upgrade >/work/live-upgrade.stdout \
    2>/work/live-upgrade.stderr
live_upgrade_status=$?
set -e
[[ "${live_upgrade_status}" -eq 1 && ! -s /work/live-upgrade.stdout ]] ||
    fail 'live upgrade did not fail closed'
[[ "$(cat /work/live-upgrade.stderr)" == \
    'orna: the default Orna instance is running' ]] || fail 'live diagnostic changed'

run_as_orna python3 - <<'PY'
import socket
import struct

SOCKET = '/run/orna/default/postgres/.s.PGSQL.5432'

def receive(sock):
    kind = sock.recv(1)
    if not kind:
        raise RuntimeError('unexpected PostgreSQL EOF')
    length = receive_exact(sock, 4)
    size = struct.unpack('!I', length)[0]
    if size < 4 or size > 16 * 1024 * 1024:
        raise RuntimeError('invalid PostgreSQL message length')
    return kind, receive_exact(sock, size - 4)

def receive_exact(sock, size):
    result = bytearray()
    while len(result) < size:
        chunk = sock.recv(size - len(result))
        if not chunk:
            raise RuntimeError('unexpected PostgreSQL EOF')
        result.extend(chunk)
    return bytes(result)

def fields(payload):
    values = {}
    offset = 0
    while offset < len(payload) and payload[offset] != 0:
        code = chr(payload[offset])
        end = payload.index(0, offset + 1)
        values[code] = payload[offset + 1:end].decode()
        offset = end + 1
    return values

sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.settimeout(10)
sock.connect(SOCKET)
parameters = b'user\0orna_kernel\0database\0orna\0client_encoding\0UTF8\0\0'
sock.sendall(struct.pack('!II', len(parameters) + 8, 196608) + parameters)
authenticated = False
key = False
while True:
    kind, payload = receive(sock)
    if kind == b'R' and payload == struct.pack('!I', 0):
        authenticated = True
    elif kind == b'S' and authenticated:
        pass
    elif kind == b'K' and authenticated and len(payload) == 8:
        key = True
    elif kind == b'Z' and authenticated and key and payload == b'I':
        break
    else:
        raise RuntimeError(f'unexpected startup frame {kind!r}')

def query(sql, expected_error=None):
    payload = sql.encode() + b'\0'
    sock.sendall(b'Q' + struct.pack('!I', len(payload) + 4) + payload)
    errors = []
    rows = []
    while True:
        kind, body = receive(sock)
        if kind == b'E':
            errors.append(fields(body))
        elif kind == b'D':
            count = struct.unpack('!H', body[:2])[0]
            offset = 2
            row = []
            for _ in range(count):
                length = struct.unpack('!i', body[offset:offset + 4])[0]
                offset += 4
                row.append(None if length == -1 else body[offset:offset + length].decode())
                if length >= 0:
                    offset += length
            if offset != len(body):
                raise RuntimeError('malformed data row')
            rows.append(row)
        elif kind in (b'T', b'C'):
            pass
        elif kind == b'Z' and body == b'I':
            break
        else:
            raise RuntimeError(f'unexpected query frame {kind!r}')
    if expected_error is None:
        if errors:
            raise RuntimeError(f'unexpected SQL error {errors!r}')
        return rows
    if len(errors) != 1 or errors[0].get('C') != '0A000' or errors[0].get('M') != expected_error:
        raise RuntimeError(f'wrong SQL rejection {errors!r}')
    if rows:
        raise RuntimeError('rejected SQL returned rows')

facts = query("SELECT current_user, current_database(), current_setting('server_version_num'), current_setting('data_checksums'), (SELECT count(*)::text FROM pg_language WHERE lanname = 'plpgsql')")
if facts != [['orna_kernel', 'orna', '180004', 'on', '0']]:
    raise RuntimeError(f'wrong cluster facts {facts!r}')

for sql, diagnostic in [
    ("LOAD '/does/not/exist'", 'Orna does not permit SQL LOAD'),
    ("COPY (SELECT 1) TO PROGRAM 'false'", 'Orna does not permit COPY PROGRAM'),
    ('CREATE EXTENSION plpgsql', 'Orna does not permit PostgreSQL extension management'),
    ('CREATE TRUSTED PROCEDURAL LANGUAGE orna_forbidden HANDLER no_such_handler', 'Orna does not permit procedural language creation'),
    ('DO $$ BEGIN END $$', 'Orna does not permit anonymous procedural blocks'),
    ("CREATE FUNCTION orna_c_guard() RETURNS integer AS '/does/not/exist', 'entry' LANGUAGE C", 'Orna does not permit C or internal language function or procedure definitions'),
]:
    query(sql, diagnostic)
sock.sendall(b'X' + struct.pack('!I', 4))
sock.close()
PY

printf 'SELECT 41 + 1 AS answer;\n\\g\n\\q\n' >/work/shell.input
script --quiet --return --command \
    "/usr/bin/env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin /usr/bin/setpriv --reuid=${orna_uid} --regid=${orna_gid} --clear-groups -- /usr/bin/orna server backend-shell" \
    /work/shell.raw </work/shell.input >/work/shell.stdout
tr -d '\r' </work/shell.raw | sed -e 's/orna=> //g' -e 's/orna-> //g' \
    >/work/shell.transcript
grep -Fx 'answer' /work/shell.transcript >/dev/null || fail 'native shell omitted header'
grep -Fx '42' /work/shell.transcript >/dev/null || fail 'native shell omitted row'
grep -Fx 'COMMAND SELECT 1' /work/shell.transcript >/dev/null ||
    fail 'native shell omitted command tag'
grep -Fx 'TRANSACTION I' /work/shell.transcript >/dev/null ||
    fail 'native shell omitted transaction state'

stop_server "${server_process}"
run_as_orna /usr/bin/orna server upgrade >/work/stopped-upgrade.stdout \
    2>/work/stopped-upgrade.stderr
[[ ! -s /work/stopped-upgrade.stdout && ! -s /work/stopped-upgrade.stderr ]] ||
    fail 'current-engine no-op upgrade produced output'

original_manifest=$(cat "${manifest}")
printf '%s\n' "${original_manifest}" | sed \
    's/^engine = ".*"$/engine = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"/' \
    >"${manifest}"
set +e
run_as_orna /usr/bin/orna server upgrade >/work/unsupported.stdout \
    2>/work/unsupported.stderr
unsupported_status=$?
set -e
[[ "${unsupported_status}" -eq 1 && ! -s /work/unsupported.stdout ]] ||
    fail 'unsupported engine did not fail closed'
[[ "$(cat /work/unsupported.stderr)" == \
    'orna: this Orna executable cannot upgrade the installed PostgreSQL engine' ]] ||
    fail 'unsupported-engine diagnostic changed'
printf '%s\n' "${original_manifest}" >"${manifest}"

mkdir "${instance}/upgrade"
set +e
run_as_orna /usr/bin/orna server upgrade >/work/invalid.stdout 2>/work/invalid.stderr
invalid_status=$?
set -e
[[ "${invalid_status}" -eq 1 && ! -s /work/invalid.stdout ]] ||
    fail 'invalid durable state did not fail closed'
[[ "$(cat /work/invalid.stderr)" == 'orna: the default Orna instance is invalid' ]] ||
    fail 'invalid-instance diagnostic changed'
rmdir "${instance}/upgrade"

printf '%s\n' 'format = 1' 'state = "incomplete"' >/var/lib/orna/package-state.toml
set +e
run_as_orna /usr/bin/orna server upgrade >/work/incomplete.stdout \
    2>/work/incomplete.stderr
incomplete_status=$?
set -e
[[ "${incomplete_status}" -eq 1 && ! -s /work/incomplete.stdout ]] ||
    fail 'incomplete package did not fail closed'
[[ "$(cat /work/incomplete.stderr)" == 'orna: package maintenance is incomplete' ]] ||
    fail 'package-incomplete diagnostic changed'
printf '%s\n' 'format = 1' 'state = "ready"' >/var/lib/orna/package-state.toml

: >/work/server.stdout
: >/work/server.stderr
/usr/bin/env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin \
    /usr/bin/setpriv --reuid="${orna_uid}" --regid="${orna_gid}" --clear-groups -- \
    /usr/bin/orna server run >/work/server.stdout 2>/work/server.stderr &
server_process=$!
wait_ready "${server_process}"
require_one_executable_tree
stop_server "${server_process}"
[[ "$(cat "${manifest}")" == "${original_manifest}" ]] ||
    fail 'restart changed the committed instance manifest'

printf '%s\n' '[clean-machine] one executable lifecycle passed'
TEST
