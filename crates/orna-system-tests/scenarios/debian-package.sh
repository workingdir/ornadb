#!/bin/bash
set -euo pipefail
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

repository_root=$(cd "$(dirname "$0")/../../.." && pwd -P)
changelog="${repository_root}/packaging/debian/changelog"
source="$(dpkg-parsechangelog -l"${changelog}" -S Source)"
version="$(dpkg-parsechangelog -l"${changelog}" -S Version)"
architecture="$(dpkg --print-architecture)"
[[ "${architecture}" == 'amd64' ]] || {
    printf '%s\n' '[package-test] error: package proof requires amd64' >&2
    exit 1
}
package=${1:-"${repository_root}/target/debian-package/${source}_${version}_${architecture}.deb"}

[[ "${package}" == /* && -f "${package}" && ! -L "${package}" ]] || {
    printf '%s\n' '[package-test] error: package must be one absolute regular file' >&2
    exit 1
}

scratch=$(mktemp -d "${repository_root}/target/debian-package-test.XXXXXX")
image="orna-debian-package-test:$PPID-$$"
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
ready='format = 1
state = "ready"'
incomplete='format = 1
state = "incomplete"'
configuration='format = 1
instance = "default"'

fail() {
    printf '%s\n' "[package-test] error: $*" >&2
    exit 1
}

require_state() {
    [[ -f /var/lib/orna/package-state.toml ]] || fail 'package state is absent'
    [[ "$(cat /var/lib/orna/package-state.toml)" == "$1" ]] || fail 'package state is not accepted'
    [[ "$(stat -c '%U:%G %a' /var/lib/orna/package-state.toml)" == 'root:orna 640' ]] ||
        fail 'package state metadata is not accepted'
}

require_installed() {
    [[ "$(dpkg-query -W -f='${db:Status-Status}' orna)" == installed ]] ||
        fail 'package is not installed'
    [[ "$(stat -c '%U:%G %a' /usr/bin/orna)" == 'root:root 755' ]] ||
        fail 'installed executable metadata is not accepted'
    [[ "$(cat /etc/orna/instances/default.toml)" == "$configuration" ]] ||
        fail 'default configuration bytes changed'
    [[ "$(stat -c '%U:%G %a' /var/lib/orna)" == 'root:root 755' ]] ||
        fail 'package root metadata is not accepted'
    [[ "$(stat -c '%U:%G %a' /var/lib/orna/instances)" == 'orna:orna 700' ]] ||
        fail 'instance parent metadata is not accepted'
    [[ ! -e /run/systemd/system ]] || fail 'test machine unexpectedly runs systemd'
    ! find /usr -xdev -type f \
        \( -name postgres -o -name initdb -o -name psql -o -name pg_upgrade \
        -o -name '*.so' -o -name '*.so.*' -o -name '*.a' -o -name '*.o' \) \
        -path '*orna*' -print -quit | grep -q . || fail 'PostgreSQL runtime artefact is installed'
    mapfile -t product_executables < <(
        find /usr -xdev -type f -perm /111 -path '*orna*' -print | LC_ALL=C sort
    )
    [[ "${product_executables[*]}" == /usr/bin/orna ]] ||
        fail 'installed product executable inventory is not exact'
    [[ ! -e /usr/lib/orna/libexec ]] || fail 'a private helper was installed'
    require_state "$ready"
}

make_variant() {
    local version=$1
    local destination=$2
    local root="/work/package-${version}"
    mkdir -m 0700 -p /work
    dpkg-deb --raw-extract "$package" "$root"
    sed -i "s/^Version: .*/Version: ${version}/" "$root/DEBIAN/control"
    find "$root" -exec touch -h -d '@1778528675' -- {} +
    SOURCE_DATE_EPOCH=1778528675 dpkg-deb --root-owner-group --build "$root" "$destination"
}

mkdir -m 0700 -p /work
make_variant 0.1.0-2 /work/orna-forward.deb

# Initial installation has no predecessor helper and commits ready state.
dpkg --install "$package"
require_installed
[[ "$(getent passwd orna | cut -d: -f7)" == /usr/sbin/nologin ]] ||
    fail 'service account shell is not accepted'
[[ "$(id -u orna)" != 0 && "$(id -g orna)" != 0 ]] ||
    fail 'service account is root'

install -o orna -g orna -m 0600 /dev/null /var/lib/orna/instances/retained-test-data

# An equal-version repair follows begin and complete without starting the service.
dpkg --install "$package"
require_installed

# A shared package reader excludes begin and leaves the prior ready state unchanged.
python3 - <<'PY' &
import fcntl
from pathlib import Path
import time

lock = open('/var/lib/orna/package.lock', 'r+b', buffering=0)
fcntl.lockf(lock, fcntl.LOCK_SH, 1, 0)
Path('/work/reader-ready').write_bytes(b'ready\n')
time.sleep(60)
PY
reader=$!
for _ in $(seq 1 100); do
    [[ -f /work/reader-ready ]] && break
    sleep 0.01
done
[[ -f /work/reader-ready ]] || fail 'reader did not acquire the package lock'
set +e
/usr/bin/env -i ORNA_PACKAGE_MAINTENANCE=begin /usr/bin/orna \
    >/work/reader.stdout 2>/work/reader.stderr
reader_status=$?
set -e
[[ "$reader_status" -eq 1 && ! -s /work/reader.stdout ]] ||
    fail 'reader conflict did not fail closed'
[[ "$(cat /work/reader.stderr)" == 'orna: package maintenance did not complete' ]] ||
    fail 'reader conflict diagnostic changed'
require_state "$ready"
kill "$reader"
wait "$reader" 2>/dev/null || true

# A forward update commits incomplete before unpack. Failed postinst remains incomplete;
# exact repair re-enters complete and commits ready.
dpkg --unpack /work/orna-forward.deb
require_state "$incomplete"
printf '%s\n' 'invalid = true' >/etc/orna/instances/default.toml
set +e
dpkg --configure orna >/work/configure.stdout 2>/work/configure.stderr
configure_status=$?
set -e
[[ "$configure_status" -ne 0 ]] || fail 'invalid package configuration was accepted'
require_state "$incomplete"
printf '%s\n' "$configuration" >/etc/orna/instances/default.toml
dpkg --configure orna
require_installed
[[ "$(dpkg-query -W -f='${Version}' orna)" == 0.1.0-2 ]] ||
    fail 'forward update version is not installed'

# The old package is now a real downgrade. It must fail before begin.
set +e
dpkg --install "$package" >/work/downgrade.stdout 2>/work/downgrade.stderr
downgrade_status=$?
set -e
[[ "$downgrade_status" -ne 0 ]] || fail 'package downgrade was accepted'
grep -Fx 'orna: package downgrade is not supported' /work/downgrade.stderr >/dev/null ||
    fail 'downgrade diagnostic changed'
require_installed
[[ "$(dpkg-query -W -f='${Version}' orna)" == 0.1.0-2 ]] ||
    fail 'downgrade changed the installed version'

# Repair after the rejected transaction is idempotent.
dpkg --install /work/orna-forward.deb
require_installed

# Removal leaves configuration and all persistent state, with package state incomplete.
dpkg --remove orna
[[ "$(dpkg-query -W -f='${db:Status-Status}' orna)" == config-files ]] ||
    fail 'removal state is not config-files'
[[ ! -e /usr/bin/orna && -f /etc/orna/instances/default.toml ]] ||
    fail 'removal inventory is not accepted'
require_state "$incomplete"
[[ -f /var/lib/orna/instances/retained-test-data ]] || fail 'removal deleted instance data'

# Reinstall from Config-Files reaches ready through the new package.
dpkg --install /work/orna-forward.deb
require_installed
[[ -f /var/lib/orna/instances/retained-test-data ]] || fail 'reinstall replaced instance data'

# Direct purge invokes begin, removes the conffile, and retains all persistent data.
dpkg --purge orna
[[ ! -e /usr/bin/orna && ! -e /etc/orna/instances/default.toml ]] ||
    fail 'purge retained package-owned files'
require_state "$incomplete"
[[ -f /var/lib/orna/instances/retained-test-data ]] || fail 'purge deleted instance data'

# Reinstall after purge has no predecessor helper and re-enters complete.
dpkg --install /work/orna-forward.deb
require_installed
[[ -f /var/lib/orna/instances/retained-test-data ]] ||
    fail 'post-purge reinstall replaced instance data'

printf '%s\n' '[package-test] package transaction exclusion passed'
TEST
